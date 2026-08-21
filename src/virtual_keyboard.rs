use std::os::fd::{AsFd, OwnedFd};
use std::time::Instant;

use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create};
use rustix::io;
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
use wayland::{WlKeyboardKeyState, WlKeyboardKeymapFormat};
use xkbcommon::xkb::ffi::XKB_KEYMAP_FORMAT_TEXT_V1;
use xkbcommon::xkb::{Context, Keymap};

use crate::MechanixKeyboardState;

pub struct KeymapWithFd {
    fd: OwnedFd,
    size: u32,
}

impl KeymapWithFd {
    pub fn new(text: &[u8]) -> io::Result<Self> {
        let (fd, size) = make_keymap_fd(text)?;
        Ok(Self { fd, size })
    }
    // No manual Drop impl needed — OwnedFd closes the fd when `KeymapWithFd` drops.
}

pub struct VirtualKeyboardState {
    pub start_time: Instant,
    pub keymap: Option<KeymapWithFd>,
}

/// zwp_virtual_keyboard_v1
///
/// Minimal client: once the registry roundtrip has bound the seat and the
/// virtual-keyboard manager, create a virtual keyboard and hand the compositor
/// a standard keymap.
pub fn module<S>() -> impl app::RegisteredModule<MechanixKeyboardState, S> {
    app::Module::new().on(on_pre_poll)
}

/// The seat/manager are only known after the registry roundtrip, so create the
/// virtual keyboard lazily on the first poll where both are available.
fn on_pre_poll(s: &mut MechanixKeyboardState, _: &app::PrePoll) {
    if s.globals.virtual_keyboard.is_some() {
        send_test_key(s);
        return;
    }
    let (Some(seat), Some(manager)) = (
        s.globals.seat.clone(),
        s.globals.virtual_keyboard_manager.clone(),
    ) else {
        tracing::warn!("No vkbd manager found!");
        return;
    };

    let vkbd = manager.create_virtual_keyboard(&seat);

    // Compile a standard keymap from the default rules and serialise it.
    let ctx = Context::new(0);
    let keymap = Keymap::new_from_names(&ctx, "", "", "us", "", None, 0)
        .expect("failed to compile default keymap");
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

    s.globals.virtual_keyboard = Some(vkbd);
    s.virtual_keyboard_state.keymap = Some(keymap_fd);
    tracing::info!("sent keymap to virtual keyboard");
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

fn send_test_key(s: &mut MechanixKeyboardState) {
    if s.interactivity
        .pointer
        .pressed(interactivity::pointer::MouseButton::Left)
    {
        if let Some(vkbd) = s.globals.virtual_keyboard.clone() {
            let key = 30;
            let state = WlKeyboardKeyState::Pressed;
            vkbd.key(
                (Instant::now() - s.virtual_keyboard_state.start_time).as_millis() as u32,
                key,
                state.into(),
            );
            tracing::info!("Sending key...");
        }
    } else if s
        .interactivity
        .pointer
        .just_released(interactivity::pointer::MouseButton::Left)
    {
        if let Some(vkbd) = s.globals.virtual_keyboard.clone() {
            let key = 65;
            let state = WlKeyboardKeyState::Released;
            vkbd.key(
                (Instant::now() - s.virtual_keyboard_state.start_time).as_millis() as u32,
                key,
                state.into(),
            );
            tracing::info!("Releasing key...");
        }
    }
}
