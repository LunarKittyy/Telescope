//! Lossless still-photo geometry, independent of the preview pipeline.

use libjpeg_turbo_rs::{probe, transform_jpeg_with_options, CropRegion, TransformOp, TransformOptions};
use std::io::Write;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[path = "photo/heif.rs"]
mod heif;
#[path = "photo/insert.rs"]
mod insert;
#[path = "photo/orient.rs"]
mod orient;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PhotoTransform {
    pub flip_h: bool,
    pub flip_v: bool,
    pub rotation: i32,
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
}

impl Default for PhotoTransform {
    fn default() -> Self {
        Self { flip_h: false, flip_v: false, rotation: 0, zoom: 1.0, pan_x: 0.0, pan_y: 0.0 }
    }
}

impl PhotoTransform {
    pub fn for_lens(&self, ratio: f64) -> Self {
        if self.zoom <= 1.0 { return self.clone(); }
        let phone = crate::view::PhoneZoom { ratio: finite(ratio, 1.0).max(1.0), crop: 1.0, x: 0.5, y: 0.5 };
        let (zoom, pan_x, pan_y) = crate::view::rest_within(self.zoom, self.pan_x, self.pan_y, &phone, true).unwrap_or((1.0, 0.0, 0.0));
        Self { zoom, pan_x, pan_y, ..self.clone() }
    }
}

fn operation(t: &PhotoTransform) -> TransformOp {
    use TransformOp::*;
    match (t.rotation.rem_euclid(360), t.flip_h, t.flip_v) {
        (0, false, false) | (180, true, true) => None,
        (0, true, false) | (180, false, true) => HFlip,
        (0, false, true) | (180, true, false) => VFlip,
        (0, true, true) | (180, false, false) => Rot180,
        (90, false, false) | (270, true, true) => Rot90,
        (270, false, false) | (90, true, true) => Rot270,
        (90, true, false) | (270, false, true) => Transverse,
        (90, false, true) | (270, true, false) => Transpose,
        _ => None,
    }
}

/// How big a picture is and how big the units it is coded in are (MCUs), read from the largest sampling factors in its frame header
struct Frame {
    width: usize,
    height: usize,
    mcu_width: usize,
    mcu_height: usize,
    components: usize,
}

fn frame(data: &[u8]) -> Result<Frame, String> {
    let unreadable = || "The photo's size can't be read".to_string();
    for (marker, start, end) in app_segments(data) {
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let segment = &data[start..end];
            let count = usize::from(*segment.get(5).ok_or_else(unreadable)?);
            let samples = segment.get(6..6 + count * 3).ok_or_else(unreadable)?;
            let (height, width) = (usize::from(u16::from_be_bytes([segment[1], segment[2]])), usize::from(u16::from_be_bytes([segment[3], segment[4]])));
            let across = samples.chunks(3).map(|c| usize::from(c[1] >> 4)).max().unwrap_or(0);
            let down = samples.chunks(3).map(|c| usize::from(c[1] & 15)).max().unwrap_or(0);
            if width == 0 || height == 0 || across == 0 || down == 0 { break; }
            return Ok(Frame { width, height, mcu_width: across * 8, mcu_height: down * 8, components: count });
        }
    }
    Err(unreadable())
}

/// The edges (right, bottom) a turn can only do cleanly when they hold whole MCUs, which is where jpegtran trims
fn edges_to_trim(op: TransformOp) -> (bool, bool) {
    match op {
        TransformOp::HFlip | TransformOp::Rot270 => (true, false),
        TransformOp::VFlip | TransformOp::Rot90 => (false, true),
        TransformOp::Rot180 | TransformOp::Transverse => (true, true),
        TransformOp::None | TransformOp::Transpose => (false, false),
    }
}

fn crop_only(data: &[u8], crop: CropRegion) -> Result<Vec<u8>, String> {
    transform_jpeg_with_options(data, &TransformOptions { op: TransformOp::None, trim: false, crop: Some(crop), ..Default::default() }).map_err(|e| e.to_string())
}

/// The trimming is done here, by cropping, because the codec's own trimming also cuts the padding off the edge it didn't trim and leaves a scan that is too short
fn transform(data: &[u8], op: TransformOp, crop: Option<CropRegion>) -> Result<Vec<u8>, String> {
    let (trim_width, trim_height) = edges_to_trim(op);
    let trimmed;
    let mut data = data;
    if crop.is_none() && (trim_width || trim_height) {
        let f = frame(data)?;
        // A picture smaller than one MCU across can't be turned cleanly, and is left as it is
        if (trim_width && f.width < f.mcu_width) || (trim_height && f.height < f.mcu_height) {
            return Err("Photo is smaller than one JPEG MCU".into());
        }
        let width = if trim_width { f.width / f.mcu_width * f.mcu_width } else { f.width };
        let height = if trim_height { f.height / f.mcu_height * f.mcu_height } else { f.height };
        if (width, height) != (f.width, f.height) {
            trimmed = crop_only(data, CropRegion { x: 0, y: 0, width, height })?;
            data = &trimmed;
        }
    }
    transform_jpeg_with_options(data, &TransformOptions { op, trim: false, crop, ..Default::default() }).map_err(|e| e.to_string())
}

/// Decodes the whole picture, and checks that it has the size it says and all of its MCUs: a scan that is cut short is not a photo
fn verify_jpeg(data: &[u8]) -> Result<(), String> {
    let broken = |what: String| format!("The edited photo isn't a whole picture: {what}");
    let f = frame(data)?;
    let image = libjpeg_turbo_rs::decompress(data).map_err(|e| broken(e.to_string()))?;
    if (image.width, image.height) != (f.width, f.height) {
        return Err(broken(format!("{}x{} instead of {}x{}", image.width, image.height, f.width, f.height)));
    }
    let coefficients = libjpeg_turbo_rs::read_coefficients(data).map_err(|e| broken(e.to_string()))?;
    for c in &coefficients.components {
        let (wide, high) = if f.components == 1 {
            (f.width.div_ceil(8), f.height.div_ceil(8))
        } else {
            (f.width.div_ceil(f.mcu_width) * usize::from(c.h_sampling), f.height.div_ceil(f.mcu_height) * usize::from(c.v_sampling))
        };
        if c.blocks_x < wide || c.blocks_y < high {
            return Err(broken("it has fewer blocks than its size needs".into()));
        }
    }
    Ok(())
}

fn finite(value: f64, default: f64) -> f64 {
    if value.is_finite() { value } else { default }
}

fn crop_axis(size: usize, block: usize, zoom: f64, pan: f64) -> Result<(usize, usize), String> {
    let blocks = size / block;
    if blocks == 0 { return Err("Photo is smaller than one JPEG MCU".into()); }
    let length = ((size as f64 / zoom / block as f64).round() as usize).clamp(1, blocks);
    let origin = (((size as f64 - size as f64 / zoom) * (pan + 1.0) * 0.5 / block as f64).round() as usize).min(blocks - length);
    Ok((origin * block, length * block))
}

fn crop_region(data: &[u8], t: &PhotoTransform) -> Result<Option<CropRegion>, String> {
    let zoom = finite(t.zoom, 1.0).max(1.0);
    if zoom <= 1.0 { return Ok(None); }
    let f = frame(data)?;
    let (x, width) = crop_axis(f.width, f.mcu_width, zoom, finite(t.pan_x, 0.0).clamp(-1.0, 1.0))?;
    let (y, height) = crop_axis(f.height, f.mcu_height, zoom, finite(t.pan_y, 0.0).clamp(-1.0, 1.0))?;
    Ok(Some(CropRegion { x, y, width, height }))
}

/// Where the TIFF data of each EXIF segment of the picture's own headers sits
fn exif_places(data: &[u8]) -> Vec<(usize, usize)> {
    app_segments(data).into_iter().filter(|(marker, start, end)| *marker == 0xe1 && data[*start..*end].starts_with(b"Exif\0\0")).map(|(_, start, end)| (start + 6, end)).collect()
}

/// The marker, and where the data after the length starts and ends, of each segment before the picture's scan
fn app_segments(data: &[u8]) -> Vec<(u8, usize, usize)> {
    let mut found = Vec::new();
    let mut at = 2;
    while at + 4 <= data.len() && data[at] == 0xff {
        let marker = data[at + 1];
        if marker == 0xda || marker == 0xd9 { break; }
        if marker == 0xff { at += 1; continue; }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) { at += 2; continue; }
        let length = usize::from(u16::from_be_bytes([data[at + 2], data[at + 3]]));
        let Some(end) = at.checked_add(2 + length).filter(|end| length >= 2 && *end <= data.len()) else { break };
        found.push((marker, at + 4, end));
        at = end;
    }
    found
}

fn normalise_exif(data: &mut [u8]) {
    for (start, end) in exif_places(data) {
        patch_orientation(&mut data[start..end], |_| 1, true);
    }
}

/// Gives the orientation tag the value `new` makes of the one it has and returns the old one, and clears the embedded thumbnail if `drop_thumbnail`
fn patch_orientation(tiff: &mut [u8], new: impl Fn(u16) -> u16, drop_thumbnail: bool) -> Option<u16> {
    if tiff.len() < 8 { return None; }
    let le = match &tiff[..2] { b"II" => true, b"MM" => false, _ => return None };
    let u16_at = |data: &[u8], at: usize| -> Option<u16> {
        let bytes = data.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(bytes) } else { u16::from_be_bytes(bytes) })
    };
    let bytes = tiff[4..8].try_into().unwrap();
    let ifd = if le { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) } as usize;
    let count = u16_at(tiff, ifd)?;
    let mut found = None;
    for index in 0..usize::from(count) {
        let Some(at) = ifd.checked_add(2 + index * 12) else { return found };
        if at.checked_add(12).is_none_or(|end| end > tiff.len()) { return found; }
        let count_bytes = tiff[at + 4..at + 8].try_into().unwrap();
        let values = if le { u32::from_le_bytes(count_bytes) } else { u32::from_be_bytes(count_bytes) };
        if u16_at(tiff, at) == Some(0x0112) && u16_at(tiff, at + 2) == Some(3) && values == 1 {
            let old = u16_at(tiff, at + 8).unwrap_or(1);
            found.get_or_insert(old);
            let value = new(old);
            tiff[at + 8..at + 10].copy_from_slice(&if le { value.to_le_bytes() } else { value.to_be_bytes() });
        }
    }
    // A transformed photo must not advertise a stale embedded thumbnail.
    if drop_thumbnail {
        if let Some(next) = ifd.checked_add(2 + usize::from(count) * 12).and_then(|at| tiff.get_mut(at..at + 4)) { next.fill(0); }
    }
    found
}

/// A JPEG that holds more than one picture, like an Ultra HDR photo with its gain map. Re-encoding the first would drop or break the others.
fn is_multi_picture(data: &[u8]) -> bool {
    app_segments(data).into_iter().any(|(marker, start, end)| {
        let segment = &data[start..end];
        let contains = |needle: &[u8]| segment.windows(needle.len()).any(|w| w == needle);
        (marker == 0xe2 && (segment.starts_with(b"MPF\0") || contains(b"urn:iso:std:iso:ts:21496")))
            || (marker == 0xe1 && segment.starts_with(b"http://ns.adobe.com/xap/1.0/\0") && (contains(b"hdrgm") || contains(b"GainMap")))
    })
}

/// A photo with more pictures inside is turned by its orientation tag alone, so the gain map keeps matching the picture. With `add_missing`, a photo that has no tag gets one.
fn turn_by_tag(data: &[u8], op: TransformOp, add_missing: bool) -> Result<Vec<u8>, String> {
    let mut output = data.to_vec();
    let current = exif_places(&output).into_iter().find_map(|(start, end)| patch_orientation(&mut output[start..end].to_vec(), |v| v, false));
    let Some(current) = current else {
        return if add_missing { insert::add_orientation(data, orient::exif_value(op)) } else { Err("The photo has no orientation tag to turn".into()) };
    };
    let now = u8::try_from(current).ok().and_then(TransformOp::from_exif_orientation).unwrap_or(TransformOp::None);
    let value = orient::exif_value(orient::then(now, op));
    for (start, end) in exif_places(&output) {
        patch_orientation(&mut output[start..end], |_| value, true);
    }
    Ok(output)
}

fn apply_unchecked(data: &[u8], t: &PhotoTransform) -> Result<Vec<u8>, String> {
    if heif::is_heif(data) { return heif::turn(data, operation(t)); }
    if !data.starts_with(&[0xff, 0xd8, 0xff]) { return Ok(data.to_vec()); }
    if is_multi_picture(data) {
        let op = operation(t);
        return if op == TransformOp::None { Ok(data.to_vec()) } else { turn_by_tag(data, op, true) };
    }
    let op = operation(t);
    match lossless_jpeg(data, t).and_then(|output| if output == data { Ok(output) } else { verify_jpeg(&output).map(|()| output) }) {
        Ok(output) => Ok(output),
        Err(why) => match turn_by_tag(data, op, false) {
            // The picture can't be turned in place, but it can still show the right way up
            Ok(output) if op != TransformOp::None => {
                log::warn!("Couldn't turn the photo itself ({why}), turned it by its orientation tag and left out any zoom");
                Ok(output)
            }
            _ => Err(why),
        },
    }
}

fn lossless_jpeg(data: &[u8], t: &PhotoTransform) -> Result<Vec<u8>, String> {
    let info = probe(data).map_err(|e| e.to_string())?;
    let exif = info.exif_orientation.and_then(TransformOp::from_exif_orientation).unwrap_or(TransformOp::None);
    let mut output = if exif == TransformOp::None { data.to_vec() } else { transform(data, exif, None)? };
    if let Some(crop) = crop_region(&output, t)? { output = transform(&output, TransformOp::None, Some(crop))?; }
    let op = operation(t);
    if op != TransformOp::None { output = transform(&output, op, None)?; }
    if exif != TransformOp::None || t.zoom > 1.0 || op != TransformOp::None { normalise_exif(&mut output); }
    Ok(output)
}

/// JPEG photos are turned and cropped in the coefficients, so nothing is re-encoded. HEIC and photos with a gain map only get their orientation changed, never a crop.
pub fn apply_bytes(data: &[u8], t: &PhotoTransform) -> Result<Vec<u8>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| apply_unchecked(data, t))).unwrap_or_else(|_| Err("The photo couldn't be read".into()))
}

/// Writes `bytes` over `path` by writing a file beside it and renaming that over it, so a failure at any point leaves the photo as it was
fn replace_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.edit"));
    let _ = std::fs::remove_file(&temporary);
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if let Ok(meta) = std::fs::metadata(path) { let _ = std::fs::set_permissions(&temporary, meta.permissions()); }
        std::fs::rename(&temporary, path)
    })();
    written.map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        e.to_string()
    })
}

/// Turns and crops the photo in the file. The photo is only ever replaced by a finished, checked one: when anything fails it stays as it is, and the error says why.
pub fn apply_file(path: &Path, t: &PhotoTransform) -> Result<(), String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
    let name = name.strip_suffix(".part").unwrap_or(&name);
    let extension = name.rsplit('.').next().unwrap_or("");
    if matches!(extension, "dng" | "raw" | "cr2" | "cr3" | "nef" | "nrw" | "arw" | "raf" | "rw2" | "orf" | "pef") { return Ok(()); }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    if !data.starts_with(&[0xff, 0xd8, 0xff]) && !heif::is_heif(&data) { return Err("Only JPEG and HEIC photos can be turned".into()); }
    let output = apply_bytes(&data, t)?;
    if output != data { replace_file(path, &output)?; }
    Ok(())
}

/// `apply_file` for a photo that must be kept whatever happens to the edit: if it can't be edited it stays as the camera took it, and the reason is logged and returned
pub fn apply_file_or_keep(path: &Path, t: &PhotoTransform) -> Option<String> {
    let why = apply_file(path, t).err()?;
    log::warn!("Kept {} as the camera took it, it couldn't be turned or cropped: {why}", path.file_name().unwrap_or_default().to_string_lossy());
    Some(why)
}

#[cfg(test)]
#[path = "photo/tests.rs"]
mod tests;
