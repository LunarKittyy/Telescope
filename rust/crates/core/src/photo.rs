//! Lossless still-photo geometry, independent of the preview pipeline.

use libjpeg_turbo_rs::{probe, transform_jpeg_with_options, CropRegion, TransformOp, TransformOptions};
use serde::{Deserialize, Serialize};
use std::path::Path;

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

fn transform(data: &[u8], op: TransformOp, crop: Option<CropRegion>) -> Result<Vec<u8>, String> {
    transform_jpeg_with_options(data, &TransformOptions { op, trim: true, crop, ..Default::default() }).map_err(|e| e.to_string())
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
    let info = probe(data).map_err(|e| e.to_string())?;
    let (bw, bh) = if info.components == 1 {
        (8, 8)
    } else if info.subsampling == libjpeg_turbo_rs::Subsampling::Unknown {
        let coefficients = libjpeg_turbo_rs::read_coefficients(data).map_err(|e| e.to_string())?;
        (coefficients.components.iter().map(|c| usize::from(c.h_sampling) * 8).max().unwrap_or(8),
         coefficients.components.iter().map(|c| usize::from(c.v_sampling) * 8).max().unwrap_or(8))
    } else {
        (info.subsampling.mcu_width_blocks() * 8, info.subsampling.mcu_height_blocks() * 8)
    };
    let (x, width) = crop_axis(info.width, bw, zoom, finite(t.pan_x, 0.0).clamp(-1.0, 1.0))?;
    let (y, height) = crop_axis(info.height, bh, zoom, finite(t.pan_y, 0.0).clamp(-1.0, 1.0))?;
    Ok(Some(CropRegion { x, y, width, height }))
}

fn normalise_exif(data: &mut [u8]) {
    let mut at = 2;
    while at + 4 <= data.len() && data[at] == 0xff {
        let marker = data[at + 1];
        if marker == 0xda || marker == 0xd9 { break; }
        if marker == 0xff { at += 1; continue; }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) { at += 2; continue; }
        let length = usize::from(u16::from_be_bytes([data[at + 2], data[at + 3]]));
        let Some(end) = at.checked_add(2 + length).filter(|end| length >= 2 && *end <= data.len()) else { break };
        if marker == 0xe1 && data[at + 4..end].starts_with(b"Exif\0\0") {
            let tiff = &mut data[at + 10..end];
            patch_orientation(tiff);
        }
        at = end;
    }
}

fn patch_orientation(tiff: &mut [u8]) {
    if tiff.len() < 8 { return; }
    let le = match &tiff[..2] { b"II" => true, b"MM" => false, _ => return };
    let u16_at = |data: &[u8], at: usize| -> Option<u16> {
        let bytes = data.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(bytes) } else { u16::from_be_bytes(bytes) })
    };
    let bytes = tiff[4..8].try_into().unwrap();
    let ifd = if le { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) } as usize;
    let Some(count) = u16_at(tiff, ifd) else { return };
    for index in 0..usize::from(count) {
        let Some(at) = ifd.checked_add(2 + index * 12) else { return };
        if at.checked_add(12).is_none_or(|end| end > tiff.len()) { return; }
        let count_bytes = tiff[at + 4..at + 8].try_into().unwrap();
        let values = if le { u32::from_le_bytes(count_bytes) } else { u32::from_be_bytes(count_bytes) };
        if u16_at(tiff, at) == Some(0x0112) && u16_at(tiff, at + 2) == Some(3) && values == 1 {
            tiff[at + 8..at + 10].copy_from_slice(&if le { 1_u16.to_le_bytes() } else { 1_u16.to_be_bytes() });
        }
    }
    // A transformed photo must not advertise a stale embedded thumbnail.
    if let Some(next) = ifd.checked_add(2 + usize::from(count) * 12).and_then(|at| tiff.get_mut(at..at + 4)) { next.fill(0); }
}

pub fn apply_bytes(data: &[u8], t: &PhotoTransform) -> Result<Vec<u8>, String> {
    if !data.starts_with(&[0xff, 0xd8, 0xff]) { return Ok(data.to_vec()); }
    let info = probe(data).map_err(|e| e.to_string())?;
    let exif = info.exif_orientation.and_then(TransformOp::from_exif_orientation).unwrap_or(TransformOp::None);
    let mut output = if exif == TransformOp::None { data.to_vec() } else { transform(data, exif, None)? };
    if let Some(crop) = crop_region(&output, t)? { output = transform(&output, TransformOp::None, Some(crop))?; }
    let op = operation(t);
    if op != TransformOp::None { output = transform(&output, op, None)?; }
    if exif != TransformOp::None || t.zoom > 1.0 || op != TransformOp::None { normalise_exif(&mut output); }
    Ok(output)
}

pub fn apply_file(path: &Path, t: &PhotoTransform) -> Result<(), String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
    let name = name.strip_suffix(".part").unwrap_or(&name);
    let extension = name.rsplit('.').next().unwrap_or("");
    if matches!(extension, "dng" | "raw" | "cr2" | "cr3" | "nef" | "nrw" | "arw" | "raf" | "rw2" | "orf" | "pef") { return Ok(()); }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    if !data.starts_with(&[0xff, 0xd8, 0xff]) { return Err("Non-RAW photos require JPEG for lossless transforms".into()); }
    let output = apply_bytes(&data, t)?;
    if output != data { std::fs::write(path, output).map_err(|e| e.to_string())?; }
    Ok(())
}

#[cfg(test)]
#[path = "photo/tests.rs"]
mod tests;
