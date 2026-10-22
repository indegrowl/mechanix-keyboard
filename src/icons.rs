use mecha_wayland::prelude::*;
use std::collections::HashMap;

macro_rules! icon {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!("../resources/icons/", $name, ".svg")),
        )
    };
}

/// Every bundled SVG icon, keyed by the name layouts refer to it by.
const BUNDLED: &[(&str, &[u8])] = &[
    icon!("edit-clear-symbolic"),
    icon!("face-smile-symbolic"),
    icon!("go-down-symbolic"),
    icon!("go-next-symbolic"),
    icon!("go-previous-symbolic"),
    icon!("go-up-symbolic"),
    icon!("key-enter-symbolic"),
    icon!("key-shift-symbolic"),
    icon!("preferences-system-symbolic"),
];

#[derive(Debug)]
pub struct Icons {
    icons: HashMap<String, SpriteId>,
}

impl Resource for Icons {}

impl Icons {
    pub fn load(atlas: &mut Atlas, size: u32) -> Self {
        let mut icons: HashMap<String, SpriteId> = HashMap::new();
        for &(name, bytes) in BUNDLED {
            let bitmap = Bitmap::from_svg(bytes, size)
                .unwrap_or_else(|e| panic!("bundled icon {name:?} loads: {e:?}"));
            let sprite = atlas.insert(Class::Icon, &bitmap).unwrap();
            icons.insert(name.to_owned(), sprite);
        }
        Self { icons }
    }

    pub fn get(&self, name: &str) -> Option<SpriteId> {
        self.icons.get(name).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_icons_load() {
        let icons = Icons::load(&mut Atlas::new(), 24);
        assert_eq!(icons.icons.len(), BUNDLED.len());
        assert!(icons.get("key-shift-symbolic").is_some());
    }
}
