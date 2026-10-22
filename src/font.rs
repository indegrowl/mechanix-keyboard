use mecha_wayland::prelude::{Atlas, FontId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Geist,
    GeistMono,
}

impl Font {
    pub fn from_name(name: &str) -> Option<Font> {
        match name {
            "geist" => Some(Font::Geist),
            "geist-mono" => Some(Font::GeistMono),
            _ => None,
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Font::Geist => include_bytes!("../resources/Geist-Regular.ttf"),
            Font::GeistMono => include_bytes!("../resources/GeistMono-Regular.ttf"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Fonts {
    geist: FontId,
    geist_mono: FontId,
}

impl Fonts {
    pub fn load(atlas: &mut Atlas) -> Self {
        let mut add = |font: Font| {
            atlas
                .add_font(font.bytes())
                .unwrap_or_else(|e| panic!("bundled font {font:?} loads: {e:?}"))
        };
        Fonts {
            geist: add(Font::Geist),
            geist_mono: add(Font::GeistMono),
        }
    }

    pub fn id(&self, font: Font) -> FontId {
        match font {
            Font::Geist => self.geist,
            Font::GeistMono => self.geist_mono,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_names_map_to_fonts() {
        assert_eq!(Font::from_name("geist"), Some(Font::Geist));
        assert_eq!(Font::from_name("geist-mono"), Some(Font::GeistMono));
    }

    #[test]
    fn unknown_name_is_none() {
        assert_eq!(Font::from_name("roboto-mono"), None);
        assert_eq!(Font::from_name("Geist"), None);
    }

    #[test]
    fn bundled_fonts_load() {
        let fonts = Fonts::load(&mut Atlas::new());
        assert_ne!(fonts.id(Font::Geist), fonts.id(Font::GeistMono));
    }
}
