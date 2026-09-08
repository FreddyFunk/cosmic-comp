// SPDX-License-Identifier: GPL-3.0-only

use smithay::{
    reexports::wayland_server::protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
    wayland::{
        compositor::with_states,
        shm::with_buffer_contents,
        xdg_toplevel_icon::{ToplevelIconCachedState, XdgToplevelIconHandler},
    },
};

use crate::{state::State, wayland::protocols::toplevel_info::WindowIcon};

fn shm_pixels_to_rgba(
    data: &[u8],
    width: i32,
    height: i32,
    stride: i32,
    format: wl_shm::Format,
) -> Option<Vec<u8>> {
    if width <= 0 || height <= 0 {
        return None;
    }
    if !matches!(format, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888) {
        return None;
    }

    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    if width.checked_mul(height)? > 4 * 1024 * 1024 {
        return None;
    }
    let stride = usize::try_from(stride).ok()?;
    let row_len = width.checked_mul(4)?;
    if stride < row_len {
        return None;
    }
    let len = height.checked_mul(row_len)?;
    let required = (height - 1).checked_mul(stride)?.checked_add(row_len)?;
    if data.len() < required {
        return None;
    }

    let mut rgba = Vec::with_capacity(len);
    for row in data.chunks(stride).take(height) {
        for pixel in row[..row_len].chunks_exact(4) {
            let argb = u32::from_ne_bytes(pixel.try_into().ok()?);
            let alpha = if format == wl_shm::Format::Xrgb8888 {
                255
            } else {
                (argb >> 24) as u8
            };
            let unpremultiply = |channel: u8| {
                if alpha == 0 {
                    0
                } else {
                    u8::try_from(
                        (u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha),
                    )
                    .unwrap_or(255)
                }
            };
            rgba.extend_from_slice(&[
                unpremultiply((argb >> 16) as u8),
                unpremultiply((argb >> 8) as u8),
                unpremultiply(argb as u8),
                alpha,
            ]);
        }
    }
    Some(rgba)
}

fn shm_layout(
    width: i32,
    height: i32,
    stride: i32,
    offset: i32,
    pool_len: usize,
) -> Option<(usize, usize, usize)> {
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let stride = usize::try_from(stride).ok()?;
    let offset = usize::try_from(offset).ok()?;
    let pixels = width.checked_mul(height)?;
    if pixels == 0 || pixels > 4 * 1024 * 1024 {
        return None;
    }
    let row_len = width.checked_mul(4)?;
    if stride < row_len {
        return None;
    }
    let packed_len = height.checked_mul(row_len)?;
    let span = (height - 1).checked_mul(stride)?.checked_add(row_len)?;
    let end = offset.checked_add(span)?;
    (end <= pool_len).then_some((row_len, end, packed_len))
}

fn buffer_to_icon(buffer: &WlBuffer) -> Option<WindowIcon> {
    with_buffer_contents(buffer, |ptr, pool_len, metadata| {
        let offset = usize::try_from(metadata.offset).ok()?;
        let stride = usize::try_from(metadata.stride).ok()?;
        let height = usize::try_from(metadata.height).ok()?;
        let (row_len, _, packed_len) = shm_layout(
            metadata.width,
            metadata.height,
            metadata.stride,
            metadata.offset,
            pool_len,
        )?;

        let mut bytes = vec![0; packed_len];
        for row in 0..height {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    ptr.add(offset + row * stride),
                    bytes.as_mut_ptr().add(row * row_len),
                    row_len,
                );
            }
        }
        let pixels = shm_pixels_to_rgba(
            &bytes,
            metadata.width,
            metadata.height,
            i32::try_from(row_len).ok()?,
            metadata.format,
        )?;
        WindowIcon::from_rgba(
            u32::try_from(metadata.width).ok()?,
            u32::try_from(metadata.height).ok()?,
            pixels,
        )
    })
    .ok()?
}

fn choose_surface_icon(
    name: Option<String>,
    name_resolved: bool,
    buffer: Option<WindowIcon>,
) -> Option<WindowIcon> {
    let name = name.filter(|name| !name.is_empty());
    if name_resolved {
        return name.map(WindowIcon::Name);
    }
    buffer
}

fn icon_name_resolves(name: &str) -> bool {
    let theme = cosmic::icon_theme::default();
    freedesktop_icons::lookup(name)
        .with_size(128)
        .with_theme(&theme)
        .with_cache()
        .find()
        .is_some()
}

pub(super) fn icon_for_surface(surface: &WlSurface) -> Option<WindowIcon> {
    with_states(surface, |states| {
        let mut cached = states.cached_state.get::<ToplevelIconCachedState>();
        let icon = cached.current();

        let name = icon.icon_name().map(str::to_owned);
        let buffer = icon
            .buffers()
            .iter()
            .filter_map(|(buffer, scale)| {
                let data = with_buffer_contents(buffer, |_, _, data| data).ok()?;
                let logical_size =
                    u32::try_from(data.width).ok()? / u32::try_from((*scale).max(1)).ok()?;
                Some((
                    logical_size.abs_diff(128),
                    std::cmp::Reverse(logical_size),
                    buffer,
                ))
            })
            .min_by_key(|(distance, size, _)| (*distance, *size))
            .and_then(|(_, _, buffer)| buffer_to_icon(buffer));

        let name_resolved = name.as_deref().is_some_and(icon_name_resolves);
        choose_surface_icon(name, name_resolved, buffer)
    })
}

impl XdgToplevelIconHandler for State {}
