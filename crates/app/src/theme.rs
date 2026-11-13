use gpui::{rgb, Hsla, WindowAppearance};

/// macOS のシステム色に合わせた配色。システムの外観(ライト/ダーク)に追従する
#[derive(Clone, Copy)]
pub struct Theme {
    pub content: Hsla,
    pub sidebar: Hsla,
    pub text: Hsla,
    pub text2: Hsla,
    pub text3: Hsla,
    /// カードの塗り。文字色を薄く重ねるので、ライトでもダークでも同じ式で輪郭が出る
    pub card: Hsla,
    pub card_border: Hsla,
    pub separator: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
    pub accent: Hsla,
    pub green: Hsla,
    pub orange: Hsla,
    pub red: Hsla,
    pub button: Hsla,
    pub banner: Hsla,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ink(base: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..base }
}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }

    fn dark() -> Self {
        let fg = c(0xffffff);
        Theme {
            content: c(0x262628),
            sidebar: c(0x1e1e20),
            text: c(0xf2f2f4),
            text2: ink(fg, 0.55),
            text3: ink(fg, 0.32),
            card: ink(fg, 0.045),
            card_border: ink(fg, 0.07),
            separator: ink(fg, 0.08),
            hover: ink(fg, 0.05),
            selected: ink(fg, 0.11),
            accent: c(0x0a84ff),
            green: c(0x30d158),
            orange: c(0xff9f0a),
            red: c(0xff453a),
            button: ink(fg, 0.10),
            banner: c(0x323234),
        }
    }

    fn light() -> Self {
        let fg = c(0x000000);
        Theme {
            content: c(0xf6f6f7),
            sidebar: c(0xe8e8ea),
            text: c(0x1d1d1f),
            text2: ink(fg, 0.52),
            text3: ink(fg, 0.30),
            card: ink(fg, 0.035),
            card_border: ink(fg, 0.08),
            separator: ink(fg, 0.09),
            hover: ink(fg, 0.05),
            selected: ink(fg, 0.09),
            accent: c(0x007aff),
            // システム色そのままだと白地の文字が読みにくいので、文字に使う濃さに寄せてある
            green: c(0x248a3d),
            orange: c(0xc75c00),
            red: c(0xd70015),
            button: ink(fg, 0.06),
            banner: c(0xffffff),
        }
    }

    pub fn tint(color: Hsla, alpha: f32) -> Hsla {
        ink(color, alpha)
    }
}
