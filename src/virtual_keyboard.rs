use std::collections::{HashMap, HashSet};
use std::os::fd::{AsFd, OwnedFd};
use std::time::Instant;

use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create};
use rustix::io;
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
use wayland::{WlKeyboardKeyState, WlKeyboardKeymapFormat};
use xkbcommon::xkb::ffi::XKB_KEYMAP_FORMAT_TEXT_V1;
use xkbcommon::xkb::{self, Context, Keycode, Keymap, Keysym, MOD_NAME_CTRL, MOD_NAME_SHIFT};

use crate::MechanixKeyboardState;
use crate::layout::KeyAction;

pub struct KeymapWithFd {
    fd: OwnedFd,
    size: u32,
}

impl KeymapWithFd {
    pub fn new(text: &[u8]) -> io::Result<Self> {
        let (fd, size) = make_keymap_fd(text)?;
        Ok(Self { fd, size })
    }
}

/// One resolved keystroke: the evdev keycode to press plus the modifier mask to
/// hold while pressing it. A level-0 keysym gets `mods == 0`; a level-1 keysym
/// (e.g. `Q`, `exclam`) gets its physical key's keycode plus the Shift mask, so
/// emission reproduces the shifted keysym without any persistent modifier.
#[derive(Debug, Clone, Copy)]
pub struct Keystroke {
    pub code: u32,
    pub mods: u32,
}

pub struct VirtualKeyboardState {
    pub start_time: Instant,
    pub keymap: Option<KeymapWithFd>,
    /// keysym → keystroke, scanned from the uploaded keymap's base and shifted
    /// levels. Empty until the keymap is sent; emission is a no-op until then.
    pub keycodes: HashMap<Keysym, Keystroke>,
    /// squeekboard modifier name → its serialized mask, derived from the keymap
    /// (e.g. `Control` → the Control mask). Only latchable modifiers are indexed.
    pub mod_masks: HashMap<String, u32>,
    /// The modifier names currently latched — armed for the next keystroke, which
    /// consumes (clears) them. OSK one-shot semantics; see `toggle_latch`.
    pub latched: HashSet<String>,
}

impl VirtualKeyboardState {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            keymap: None,
            keycodes: HashMap::new(),
            mod_masks: HashMap::new(),
            latched: HashSet::new(),
        }
    }
}

pub fn module<S>() -> impl app::RegisteredModule<MechanixKeyboardState, S> {
    app::Module::new().on(on_pre_poll)
}

/// The seat/manager are only known after the registry roundtrip, so create the
/// virtual keyboard lazily on the first poll where both are available. Once it
/// exists there's nothing to do here — tapping a key drives emission directly.
fn on_pre_poll(s: &mut MechanixKeyboardState, _: &app::PrePoll) {
    if s.globals.virtual_keyboard.is_some() {
        return;
    }
    let (Some(seat), Some(manager)) = (
        s.globals.seat.clone(),
        s.globals.virtual_keyboard_manager.clone(),
    ) else {
        // Not advertised yet; try again next poll (no log — this fires every poll).
        return;
    };

    let vkbd = manager.create_virtual_keyboard(&seat);

    // Compile a standard keymap from the default rules and serialise it.
    let ctx = Context::new(0);
    let keymap = Keymap::new_from_names(&ctx, "", "", "us", "", None, 0)
        .expect("failed to compile default keymap");

    // Index the keymap's base level (layout 0, level 0) so a tapped keysym maps
    // to the evdev keycode to send. `evdev = xkb_keycode - 8`; first key wins for
    // a keysym that appears on more than one physical key.
    let keycodes = scan_keycodes(&keymap);

    let text = keymap.get_as_string(XKB_KEYMAP_FORMAT_TEXT_V1);
    let keymap_fd = match KeymapWithFd::new(text.as_bytes()) {
        Ok(keymap) => keymap,
        Err(err) => {
            tracing::warn!(%err, "failed to create keymap memfd");
            return;
        }
    };

    vkbd.keymap(
        WlKeyboardKeymapFormat::XkbV1,
        keymap_fd.fd.as_fd(),
        keymap_fd.size,
    );

    // Index the modifiers a latched key can arm, mapping the squeekboard name
    // used in the layout to the serialized mask sent over the wire. Control only
    // this pass — mirrors `layout::resolve_action`'s modifier gate.
    let mut mod_masks = HashMap::new();
    mod_masks.insert(
        "Control".to_string(),
        1u32 << keymap.mod_get_index(MOD_NAME_CTRL),
    );

    tracing::info!(mapped = keycodes.len(), "virtual keyboard ready");
    s.globals.virtual_keyboard = Some(vkbd);
    s.virtual_keyboard_state.keymap = Some(keymap_fd);
    s.virtual_keyboard_state.keycodes = keycodes;
    s.virtual_keyboard_state.mod_masks = mod_masks;
}

/// Build the `keysym → Keystroke` map from a compiled keymap's base (level 0) and
/// shifted (level 1) levels. A level-0 keysym stores an empty modifier mask; a
/// level-1 keysym stores the Shift mask, so tapping it emits the shifted keysym.
/// Levels are scanned low-to-high with `or_insert`, so the fewest-modifiers form
/// wins for a keysym present at both (e.g. a keysym that is its own shift).
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
                map.entry(ks).or_insert(Keystroke {
                    code: kc - 8,
                    mods,
                });
            }
        }
    }
    map
}

/// Emit a Key action over the virtual keyboard: a keysym (or each char of a text
/// run) becomes a keycode down+up; unwired actions just log. A keystroke emission
/// consumes any latched modifiers (one-shot); an `Unhandled` tap does not, so a
/// latch stays armed until a real key fires.
pub fn emit_action(s: &mut MechanixKeyboardState, action: &KeyAction) {
    match action {
        KeyAction::EmitKeysym(ks) => {
            emit_keysym(s, *ks);
            consume_latch(s);
        }
        KeyAction::EmitText(text) => {
            for ch in text.chars() {
                emit_keysym(s, Keysym::from_char(ch));
            }
            consume_latch(s);
        }
        KeyAction::Unhandled(name) => {
            tracing::info!(action = %name, "tapped key with no wired action");
        }
        // View switches and latches are peeled off by `window::dispatch_action`
        // before this; reaching here means a dispatch bug, not a keystroke.
        KeyAction::SetView(_) | KeyAction::ToggleView { .. } | KeyAction::LatchModifier(_) => {
            tracing::error!("non-emitting action reached the virtual keyboard; dispatch bug");
        }
    }
}

/// Toggle a modifier's latch: arm it if idle, disarm it if already armed (a
/// second tap cancels). No-op with a warning for a modifier we have no mask for.
/// The armed state is one-shot — the next keystroke emission clears it.
pub fn toggle_latch(s: &mut MechanixKeyboardState, name: &str) {
    if !s.virtual_keyboard_state.mod_masks.contains_key(name) {
        tracing::warn!(modifier = %name, "modifier has no mask; not latched");
        return;
    }
    if s.virtual_keyboard_state.latched.remove(name) {
        tracing::info!(modifier = %name, "modifier latch cleared");
    } else {
        s.virtual_keyboard_state.latched.insert(name.to_string());
        tracing::info!(modifier = %name, "modifier latched; armed for next key");
    }
}

/// The combined mask of every currently-latched modifier, to OR into a keystroke.
fn latched_mask(s: &MechanixKeyboardState) -> u32 {
    s.virtual_keyboard_state
        .latched
        .iter()
        .filter_map(|n| s.virtual_keyboard_state.mod_masks.get(n))
        .fold(0, |acc, m| acc | m)
}

/// Clear all latched modifiers after a keystroke fires (one-shot). No-op when
/// nothing is armed, so callers can invoke it unconditionally.
fn consume_latch(s: &mut MechanixKeyboardState) {
    if !s.virtual_keyboard_state.latched.is_empty() {
        s.virtual_keyboard_state.latched.clear();
        tracing::debug!("latched modifiers consumed");
    }
}

/// Send one keysym as a keycode down+up, holding the keystroke's modifiers around
/// it (e.g. Shift for an uppercase or shifted keysym) and clearing them after.
/// No-ops (with a log) if the keysym isn't in the uploaded keymap or the keyboard
/// isn't ready yet.
fn emit_keysym(s: &mut MechanixKeyboardState, ks: Keysym) {
    let name = xkb::keysym_get_name(ks);
    let Some(&stroke) = s.virtual_keyboard_state.keycodes.get(&ks) else {
        tracing::warn!(keysym = %name, "keysym absent from keymap; not typed");
        return;
    };
    let Some(vkbd) = s.globals.virtual_keyboard.clone() else {
        tracing::warn!(keysym = %name, "virtual keyboard not ready; key dropped");
        return;
    };
    // Combine the keystroke's own modifiers (e.g. Shift for a shifted keysym)
    // with any latched modifiers (e.g. an armed Ctrl) — a latched Ctrl over an
    // upper-view key yields Ctrl+Shift+key.
    let mods = stroke.mods | latched_mask(s);
    let time = (Instant::now() - s.virtual_keyboard_state.start_time).as_millis() as u32;
    // Depress the combined modifiers before the key so the receiving app maps the
    // keycode to the right level; clear them after so nothing lingers. A bare
    // level-0 key with no latch (mask 0) skips this — wire traffic is unchanged.
    if mods != 0 {
        vkbd.modifiers(mods, 0, 0, 0);
    }
    vkbd.key(time, stroke.code, WlKeyboardKeyState::Pressed.into());
    vkbd.key(time, stroke.code, WlKeyboardKeyState::Released.into());
    if mods != 0 {
        vkbd.modifiers(0, 0, 0, 0);
    }
    tracing::info!(keysym = %name, code = stroke.code, mods, "typed");
}

/// Builds a sealed, shared memfd holding `text` as a NUL-terminated
/// buffer, ready to send as `set_keymap`'s fd + size.
pub fn make_keymap_fd(text: &[u8]) -> io::Result<(OwnedFd, u32)> {
    let size = text.len() + 1; // +1 for the trailing NUL the protocol expects

    let fd: OwnedFd = memfd_create(
        c"mechanix-keyboard-keymap",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )?;

    ftruncate(&fd, size as u64)?;

    // SAFETY: `fd` is a valid memfd truncated to `size` bytes; the mapping
    // is unmapped (via the guard below) before this function returns, and
    // nothing else touches `fd` concurrently.
    let map = unsafe {
        mmap(
            std::ptr::null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::SHARED,
            &fd,
            0,
        )?
    };

    struct MmapGuard(*mut core::ffi::c_void, usize);
    impl Drop for MmapGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = munmap(self.0, self.1);
            }
        }
    }
    let guard = MmapGuard(map, size);

    // SAFETY: `guard.0` points to `size` writable, exclusively-mapped bytes.
    // We write `text.len()` bytes then the NUL, totaling exactly `size`
    // bytes, so this cannot read or write out of bounds. The mapping was
    // freshly ftruncate'd, so the trailing byte is already zero, but we
    // set it explicitly for clarity/robustness.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), guard.0.cast(), text.len());
        *(guard.0 as *mut u8).add(text.len()) = 0;
    }
    drop(guard);

    fcntl_add_seals(
        &fd,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )?;

    Ok((fd, size as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same default US keymap the running app uploads.
    fn us_keymap() -> Keymap {
        let ctx = Context::new(0);
        Keymap::new_from_names(&ctx, "", "", "us", "", None, 0).expect("compile us keymap")
    }

    #[test]
    fn shifted_keysym_carries_shift_on_the_same_key() {
        let keymap = us_keymap();
        let map = scan_keycodes(&keymap);

        // A plain lowercase letter is level 0: a bare keycode, no modifiers.
        let q = map[&Keysym::from_char('q')];
        assert_eq!(q.mods, 0, "lowercase q should need no modifiers");

        // Its uppercase reaches the *same physical key*, plus a non-empty mask —
        // this is exactly what was missing before (uppercase typed nothing).
        let cap_q = map[&Keysym::from_char('Q')];
        assert_eq!(cap_q.code, q.code, "Q must be the q key, shifted");
        assert_ne!(cap_q.mods, 0, "Q must carry the Shift mask");

        // The upper view's symbols ride the same mechanism: `!` is Shift+1.
        let one = map[&Keysym::from_char('1')];
        let bang = map[&Keysym::from_char('!')];
        assert_eq!(one.mods, 0, "digit 1 should need no modifiers");
        assert_eq!(bang.code, one.code, "! must be the 1 key, shifted");
        assert_ne!(bang.mods, 0, "! must carry the Shift mask");
    }
}
