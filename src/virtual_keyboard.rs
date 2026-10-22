//! `zwp_virtual_keyboard_v1` — the virtual-keyboard-v1 protocol client.
//!
//! `VirtualKeyboardState` is a `Resource`; the module binds the
//! `ZwpVirtualKeyboardManagerV1` global conditionally from `Globals` and creates
//! the `ZwpVirtualKeyboardV1` object once a seat is available. It also handles
//! keymap compilation, keysym→keystroke indexing and memfd keymap file creation.
//! Keycode emission lives in `vk_emit_keysym` in `main.rs`.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::time::Instant;

use rustix::fs::{MemfdFlags, SealFlags};
use xkbcommon::xkb::ffi::XKB_KEYMAP_FORMAT_TEXT_V1;
use xkbcommon::xkb::{
    self, Context, Keycode, Keymap, Keysym, MOD_NAME_ALT, MOD_NAME_CTRL, MOD_NAME_SHIFT,
};

use crate::layout::Modifier;

use mecha_wayland::prelude::*;

/// One resolved keystroke: the evdev keycode to press plus the modifier mask to
/// hold while pressing it.
#[derive(Debug, Clone, Copy)]
pub struct Keystroke {
    pub code: u32,
    pub mods: u32,
}

/// All state the virtual-keyboard-v1 client owns. Lives on the app as a
/// `Resource`, reachable from `Context` handlers via `ctx.resource_mut::<VirtualKeyboardState>()`.
pub struct VirtualKeyboardState {
    /// The `ZwpVirtualKeyboardV1` object; `None` until both the manager and a
    /// seat are available.
    pub virtual_keyboard: Option<ZwpVirtualKeyboardV1>,

    pub start_time: Instant,
    /// keysym → keystroke, scanned from the uploaded keymap's base and shifted
    /// levels. Empty until the keymap is sent; emission is a no-op until then.
    pub keycodes: HashMap<Keysym, Keystroke>,
    /// Latchable modifier → its serialized mask, derived from the keymap
    /// (e.g. `Control` → the Control mask, `Alt` → Mod1).
    pub mod_masks: HashMap<Modifier, u32>,
}

impl Default for VirtualKeyboardState {
    fn default() -> Self {
        Self {
            virtual_keyboard: None,
            start_time: Instant::now(),
            keycodes: HashMap::new(),
            mod_masks: HashMap::new(),
        }
    }
}

impl Resource for VirtualKeyboardState {}

/// Module that binds the virtual-keyboard manager global and creates the VK
/// object. Install after `WaylandModule` (which provides `Globals` and `Wayland`)
/// and after the seat is bound.
pub struct VirtualKeyboardModule;

impl Module for VirtualKeyboardModule {
    fn install(self, app: &mut App) {
        app.insert_resource(VirtualKeyboardState::default());
        init(app);
    }
}

/// Try to bind the virtual-keyboard manager and create the VK object.
/// Called at install and safe to call multiple times — it's a no-op once the
/// VK exists.
fn init(app: &mut App) {
    let seat = *app.resource::<WlSeat>();

    // The manager is an optional global — not every compositor supports
    // virtual-keyboard-v1. Bind it from `Globals` if advertised.
    let manager = app
        .resource::<Globals>()
        .find(ZwpVirtualKeyboardManagerV1::NAME)
        .cloned()
        .map(|g| {
            let (globals, mut wl) = app.query::<(Res<Globals>, ResMut<Wayland>)>();
            globals.bind::<ZwpVirtualKeyboardManagerV1>(&g, &mut wl)
        });

    let Some(manager) = manager else {
        tracing::warn!("compositor does not advertise zwp_virtual_keyboard_manager_v1");
        return;
    };

    let vkbd = {
        let mut wl = app.resource_mut::<Wayland>();
        manager.create_virtual_keyboard(&mut wl, seat)
    };

    // Compile a standard keymap from the default rules and serialise it.
    let ctx = Context::new(0);
    let keymap = Keymap::new_from_names(&ctx, "", "", "us", "", None, 0)
        .expect("failed to compile default keymap");

    let keycodes = scan_keycodes(&keymap);

    let text = keymap.get_as_string(XKB_KEYMAP_FORMAT_TEXT_V1);
    let keymap_fd = match make_keymap_fd(text.as_bytes()) {
        Ok(keymap) => keymap,
        Err(err) => {
            tracing::warn!(%err, "failed to create keymap memfd");
            return;
        }
    };

    vkbd.keymap(
        &mut app.resource_mut::<Wayland>(),
        WlKeyboardKeymapFormat::XkbV1,
        keymap_fd.fd.as_fd(),
        keymap_fd.size,
    );

    // Index the modifiers a latched key can arm, mapping each to the serialized
    // mask sent over the wire. Mirrors the modifier gate in
    // `layout::resolve_action`.
    let mod_masks = Modifier::ALL
        .into_iter()
        .map(|m| {
            let name = match m {
                Modifier::Control => MOD_NAME_CTRL,
                Modifier::Alt => MOD_NAME_ALT,
            };
            (m, 1u32 << keymap.mod_get_index(name))
        })
        .collect();

    tracing::info!(mapped = keycodes.len(), "virtual keyboard ready");
    let mut vk = app.resource_mut::<VirtualKeyboardState>();
    vk.virtual_keyboard = Some(vkbd);
    vk.keycodes = keycodes;
    vk.mod_masks = mod_masks;
}

/// Build the `keysym → Keystroke` map from a compiled keymap's base (level 0) and
/// shifted (level 1) levels.
fn scan_keycodes(keymap: &Keymap) -> HashMap<Keysym, Keystroke> {
    let shift = 1u32 << keymap.mod_get_index(MOD_NAME_SHIFT);
    let mut map = HashMap::new();
    for (level, mods) in [(0u32, 0u32), (1u32, shift)] {
        for kc in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
            if kc < 8 {
                continue;
            }
            let syms = keymap.key_get_syms_by_level(Keycode::new(kc), 0, level);
            if let Some(ks) = syms.first().copied()
                && ks.raw() != 0
            {
                map.entry(ks).or_insert(Keystroke { code: kc - 8, mods });
            }
        }
    }
    map
}

/// The insertable text a keysym produces, or `None` when it's a control key
/// (BackSpace, Return, Tab, Escape, Delete, arrows, function keys, …) with no
/// printable glyph.
pub fn keysym_text(ks: Keysym) -> Option<String> {
    let cps = xkb::keysym_to_utf32(ks);
    if cps == 0 {
        return None;
    }
    let ch = char::from_u32(cps)?;
    if ch.is_control() {
        return None;
    }
    Some(ch.to_string())
}

// Key emission lives in main.rs: `vk_emit_keysym` sends keycodes through this
// state's virtual keyboard, and `im_commit_text` is the input-method path.

// ── keymap memfd ───────────────────────────────────────────────────────────

struct KeymapWithFd {
    fd: OwnedFd,
    size: u32,
}

/// Builds a sealed memfd holding `text` as a NUL-terminated buffer, ready to
/// send as `set_keymap`'s fd + size.
///
/// The memfd is sealed against shrinking, growing and writing, so the
/// compositor can safely `mmap` it without the contents changing under it.
/// The Wayland wire layer dups the fd when the request is queued, so the
/// returned `OwnedFd` can be dropped once `keymap` has been called.
fn make_keymap_fd(text: &[u8]) -> io::Result<KeymapWithFd> {
    // +1 for the trailing NUL the protocol expects.
    let size = u32::try_from(text.len() + 1)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "keymap too large"))?;

    let fd = rustix::fs::memfd_create(
        c"mechanix-keyboard-keymap",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )?;

    let mut file = File::from(fd);
    file.write_all(text)?;
    file.write_all(&[0])?;
    let fd = OwnedFd::from(file);

    rustix::fs::fcntl_add_seals(
        &fd,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )?;

    Ok(KeymapWithFd { fd, size })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileExt;

    #[test]
    fn keymap_fd_holds_nul_terminated_text_and_is_sealed() {
        let keymap = make_keymap_fd(b"xkb_keymap {}").unwrap();
        assert_eq!(keymap.size, 14);

        let file = File::from(keymap.fd);
        let mut buf = vec![0; 14];
        file.read_exact_at(&mut buf, 0).unwrap();
        assert_eq!(buf, b"xkb_keymap {}\0");

        let seals = rustix::fs::fcntl_get_seals(&file).unwrap();
        assert!(
            seals
                .contains(SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL)
        );
        assert!(file.write_at(b"x", 0).is_err());
        assert!(file.set_len(0).is_err());
    }
}
