use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::time::Instant;

use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create};
use rustix::io;
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
use wayland::{WlKeyboardKeyState, WlKeyboardKeymapFormat};
use xkbcommon::xkb::ffi::XKB_KEYMAP_FORMAT_TEXT_V1;
use xkbcommon::xkb::{self, Context, Keycode, Keymap, Keysym};

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

pub struct VirtualKeyboardState {
    pub start_time: Instant,
    pub keymap: Option<KeymapWithFd>,
    /// keysym → evdev keycode, scanned from the uploaded keymap's base level.
    /// Empty until the keymap is sent; emission is a no-op until then.
    pub keycodes: HashMap<Keysym, u32>,
}

impl VirtualKeyboardState {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            keymap: None,
            keycodes: HashMap::new(),
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

    tracing::info!(mapped = keycodes.len(), "virtual keyboard ready");
    s.globals.virtual_keyboard = Some(vkbd);
    s.virtual_keyboard_state.keymap = Some(keymap_fd);
    s.virtual_keyboard_state.keycodes = keycodes;
}

/// Build the `keysym → evdev keycode` map from a compiled keymap's base level.
fn scan_keycodes(keymap: &Keymap) -> HashMap<Keysym, u32> {
    let mut map = HashMap::new();
    for kc in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
        if kc < 8 {
            continue;
        }
        let syms = keymap.key_get_syms_by_level(Keycode::new(kc), 0, 0);
        if let Some(ks) = syms.first().copied()
            && ks.raw() != 0
        {
            map.entry(ks).or_insert(kc - 8);
        }
    }
    map
}

/// Emit a Key action over the virtual keyboard: a keysym (or each char of a text
/// run) becomes a keycode down+up; unwired actions just log.
pub fn emit_action(s: &mut MechanixKeyboardState, action: &KeyAction) {
    match action {
        KeyAction::EmitKeysym(ks) => emit_keysym(s, *ks),
        KeyAction::EmitText(text) => {
            for ch in text.chars() {
                emit_keysym(s, Keysym::from_char(ch));
            }
        }
        KeyAction::Unhandled(name) => {
            tracing::info!(action = %name, "tapped key with no wired action");
        }
    }
}

/// Send one keysym as a keycode down+up. No-ops (with a log) if the keysym isn't
/// in the uploaded keymap or the keyboard isn't ready yet.
fn emit_keysym(s: &mut MechanixKeyboardState, ks: Keysym) {
    let name = xkb::keysym_get_name(ks);
    let Some(&code) = s.virtual_keyboard_state.keycodes.get(&ks) else {
        tracing::warn!(keysym = %name, "keysym absent from keymap; not typed");
        return;
    };
    let Some(vkbd) = s.globals.virtual_keyboard.clone() else {
        tracing::warn!(keysym = %name, "virtual keyboard not ready; key dropped");
        return;
    };
    let time = (Instant::now() - s.virtual_keyboard_state.start_time).as_millis() as u32;
    vkbd.key(time, code, WlKeyboardKeyState::Pressed.into());
    vkbd.key(time, code, WlKeyboardKeyState::Released.into());
    tracing::info!(keysym = %name, code, "typed");
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
