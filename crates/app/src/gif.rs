//! gpui の img() は GIF のコマ送りで、描画が止まっていた時間を「1 描画につき 1 コマ」で後から取り返す。
//! 背面にあったウィンドウを前に出すと、溜まった分を画面の更新ごとに送るので長いあいだ早送りになる。
//! そこで GIF は自前でコマに分け、経過時刻から今のコマを決めて 1 枚の画像として渡す。

use dockmorph_core::{tr};
use gpui::{Context, ImageSource, RenderImage, Task, Window};
use image::{codecs::gif::GifDecoder, imageops, AnimationDecoder, Frame, RgbaImage};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Dock のプラグイン(Plugin.swift)と同じ規則にして、UI と Dock で速さと見た目を揃える
const MAX_PIXELS: u32 = 256;
const MIN_DELAY_MS: u64 = 20;
const FALLBACK_DELAY_MS: u64 = 100;

struct Anim {
    frames: Vec<Arc<RenderImage>>,
    ends_ms: Vec<u64>,
}

impl Anim {
    /// 今のコマと、次のコマに変わるまでの時間
    fn at(&self, elapsed: Duration) -> (Arc<RenderImage>, Option<Duration>) {
        let total = *self.ends_ms.last().expect("a GIF has at least one frame");
        if self.frames.len() == 1 {
            return (self.frames[0].clone(), None);
        }
        let t = elapsed.as_millis() as u64 % total;
        let i = self.ends_ms.iter().position(|&end| t < end).expect("t is below total");
        (self.frames[i].clone(), Some(Duration::from_millis(self.ends_ms[i] - t)))
    }
}

enum Slot {
    Loading(#[allow(dead_code)] Task<()>),
    Ready(Arc<Anim>),
    Failed,
}

#[derive(Default)]
struct Inner {
    slots: HashMap<PathBuf, Slot>,
    used: HashSet<PathBuf>,
    next_change: Option<Duration>,
}

pub struct Gifs {
    start: Instant,
    inner: RefCell<Inner>,
    wake: RefCell<Option<Task<()>>>,
}

pub fn is_gif(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("gif"))
}

impl Gifs {
    pub fn new() -> Self {
        Gifs { start: Instant::now(), inner: RefCell::default(), wake: RefCell::default() }
    }

    /// 表示する画像。GIF の読み込み中は None(枠だけ出す)
    pub fn source<V: 'static>(&self, path: &Path, owner: impl Fn(&mut V) -> &Gifs + 'static, cx: &mut Context<V>) -> Option<ImageSource> {
        if !is_gif(path) {
            return Some(path.to_path_buf().into());
        }
        let mut inner = self.inner.borrow_mut();
        inner.used.insert(path.to_path_buf());
        match inner.slots.get(path) {
            Some(Slot::Ready(anim)) => {
                let (frame, wait) = anim.at(self.start.elapsed());
                if let Some(wait) = wait {
                    inner.next_change = Some(inner.next_change.map_or(wait, |n| n.min(wait)));
                }
                Some(frame.into())
            }
            // 読めない GIF は gpui に任せる(img() の失敗時の表示になる)
            Some(Slot::Failed) => Some(path.to_path_buf().into()),
            Some(Slot::Loading(_)) => None,
            None => {
                let p = path.to_path_buf();
                let task = cx.spawn(async move |this, cx| {
                    let decoded = cx.background_executor().spawn({
                        let p = p.clone();
                        async move { decode(&p) }
                    });
                    let slot = match decoded.await {
                        Ok(anim) => Slot::Ready(Arc::new(anim)),
                        Err(e) => {
                            eprintln!("Cannot split the GIF into frames: {}: {e:#}", p.display());
                            Slot::Failed
                        }
                    };
                    // ビューが閉じられていたら結果は要らない
                    let _ = this.update(cx, |view, cx| {
                        owner(view).inner.borrow_mut().slots.insert(p, slot);
                        cx.notify();
                    });
                });
                inner.slots.insert(path.to_path_buf(), Slot::Loading(task));
                None
            }
        }
    }

    /// 描画の最後に呼ぶ。次のコマの切り替わりで描き直しを予約し、この描画で使わなかった GIF を手放す
    pub fn finish_frame<V: 'static>(&self, window: &mut Window, cx: &mut Context<V>) {
        let mut inner = self.inner.borrow_mut();
        let used = std::mem::take(&mut inner.used);
        let unused: Vec<PathBuf> = inner.slots.keys().filter(|p| !used.contains(*p)).cloned().collect();
        for p in unused {
            if let Some(Slot::Ready(anim)) = inner.slots.remove(&p) {
                // RenderImage はウィンドウのテクスチャに載ったまま残るので、明示的に外す
                for frame in &anim.frames {
                    cx.drop_image(frame.clone(), Some(window));
                }
            }
        }
        *self.wake.borrow_mut() = inner.next_change.take().map(|wait| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
        });
    }
}

fn decode(path: &Path) -> anyhow::Result<Anim> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut frames = vec![];
    let mut ends_ms = vec![];
    let mut total = 0;
    for frame in GifDecoder::new(file)?.into_frames() {
        let frame = frame?;
        let (numer, denom) = frame.delay().numer_denom_ms();
        let ms = u64::from(numer) / u64::from(denom.max(1));
        total += if ms < MIN_DELAY_MS { FALLBACK_DELAY_MS } else { ms };
        ends_ms.push(total);
        frames.push(Arc::new(RenderImage::new(vec![Frame::new(to_bgra(shrink(frame.into_buffer())))])));
    }
    anyhow::ensure!(!frames.is_empty(), tr("コマがありません", "The GIF has no frames"));
    Ok(Anim { frames, ends_ms })
}

fn shrink(img: RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= MAX_PIXELS {
        return img;
    }
    let scale = MAX_PIXELS as f64 / w.max(h) as f64;
    let (nw, nh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
    imageops::resize(&img, nw, nh, imageops::FilterType::Triangle)
}

// gpui のテクスチャは BGRA で読む(gpui 自身の GIF 読み込みと同じ変換)
fn to_bgra(mut img: RgbaImage) -> RgbaImage {
    for px in img.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    img
}
