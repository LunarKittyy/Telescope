//! Turns a HEIF photo (HEIC) by its `irot` and `imir` properties. The coded picture is never touched.

use super::orient::{rotate_then_mirror, then};
use libjpeg_turbo_rs::TransformOp;

#[derive(Clone)]
struct Bx {
    body: usize,
    end: usize,
    kind: [u8; 4],
    /// Where the size is written and how wide it is, unless the box runs to the end of its parent
    size_at: Option<(usize, usize)>,
}

fn broken() -> String {
    "The HEIC photo is damaged".into()
}

fn uint(data: &[u8], at: usize, width: usize) -> Result<u64, String> {
    let bytes = data.get(at..at.checked_add(width).ok_or_else(broken)?).ok_or_else(broken)?;
    Ok(bytes.iter().fold(0u64, |all, b| all << 8 | u64::from(*b)))
}

fn read_boxes(data: &[u8], from: usize, to: usize) -> Result<Vec<Bx>, String> {
    let mut found = Vec::new();
    let mut at = from;
    while at < to {
        let size = uint(data, at, 4)?;
        let kind: [u8; 4] = data.get(at + 4..at + 8).and_then(|k| k.try_into().ok()).ok_or_else(broken)?;
        let (body, end, size_at) = match size {
            0 => (at + 8, to, None),
            1 => {
                let end = usize::try_from(uint(data, at + 8, 8)?).ok().and_then(|n| at.checked_add(n)).ok_or_else(broken)?;
                (at + 16, end, Some((at + 8, 8)))
            }
            n => (at + 8, at.checked_add(n as usize).ok_or_else(broken)?, Some((at, 4))),
        };
        if end > to || end < body {
            return Err(broken());
        }
        found.push(Bx { body, end, kind, size_at });
        at = end;
    }
    Ok(found)
}

fn child<'a>(kids: &'a [Bx], kind: &[u8; 4]) -> Result<&'a Bx, String> {
    kids.iter().find(|b| &b.kind == kind).ok_or_else(broken)
}

pub fn is_heif(data: &[u8]) -> bool {
    if data.get(4..8) != Some(b"ftyp") {
        return false;
    }
    let end = usize::try_from(uint(data, 0, 4).unwrap_or(0)).unwrap_or(0).min(data.len());
    let known = |brand: &[u8]| matches!(brand, b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1");
    data.get(8..12).is_some_and(known) || data.get(16..end).unwrap_or_default().chunks(4).any(known)
}

struct Entry {
    start: usize,
    end: usize,
    item: u64,
    associations: Vec<(bool, u16)>,
}

fn entries(data: &[u8], ipma: &Bx) -> Result<(Vec<Entry>, bool), String> {
    let version = *data.get(ipma.body).ok_or_else(broken)?;
    let wide = uint(data, ipma.body + 1, 3)? & 1 == 1;
    let count = uint(data, ipma.body + 4, 4)?;
    let mut at = ipma.body + 8;
    let mut found = Vec::new();
    for _ in 0..count {
        let start = at;
        let id_width = if version == 0 { 2 } else { 4 };
        let item = uint(data, at, id_width)?;
        let many = uint(data, at + id_width, 1)?;
        at += id_width + 1;
        let mut associations = Vec::new();
        for _ in 0..many {
            let (essential, index) = if wide {
                let v = uint(data, at, 2)? as u16;
                at += 2;
                (v >> 15 == 1, v & 0x7fff)
            } else {
                let v = uint(data, at, 1)? as u16;
                at += 1;
                (v >> 7 == 1, v & 0x7f)
            };
            associations.push((essential, index));
        }
        if at > ipma.end {
            return Err(broken());
        }
        found.push(Entry { start, end: at, item, associations });
    }
    Ok((found, wide))
}

fn encode(item: u64, version: u8, wide: bool, associations: &[(bool, u16)]) -> Result<Vec<u8>, String> {
    let mut out = if version == 0 { (item as u16).to_be_bytes().to_vec() } else { (item as u32).to_be_bytes().to_vec() };
    out.push(u8::try_from(associations.len()).map_err(|_| broken())?);
    for (essential, index) in associations {
        if wide {
            out.extend((index | u16::from(*essential) << 15).to_be_bytes());
        } else {
            out.push(u8::try_from(*index).ok().filter(|i| *i < 0x80).ok_or("The HEIC photo has too many properties to turn")? | u8::from(*essential) << 7);
        }
    }
    Ok(out)
}

/// The primary picture and the thumbnails that point at it
fn targets(data: &[u8], meta: &[Bx]) -> Result<Vec<u64>, String> {
    let pitm = child(meta, b"pitm")?;
    let primary = uint(data, pitm.body + 4, if *data.get(pitm.body).ok_or_else(broken)? == 0 { 2 } else { 4 })?;
    let mut items = vec![primary];
    if let Some(iref) = meta.iter().find(|b| &b.kind == b"iref") {
        let id_width = if *data.get(iref.body).ok_or_else(broken)? == 0 { 2 } else { 4 };
        for reference in read_boxes(data, iref.body + 4, iref.end)?.iter().filter(|r| &r.kind == b"thmb") {
            let from = uint(data, reference.body, id_width)?;
            let count = uint(data, reference.body + id_width, 2)?;
            let at = reference.body + id_width + 2;
            if (0..count).any(|n| uint(data, at + n as usize * id_width, id_width) == Ok(primary)) {
                items.push(from);
            }
        }
    }
    Ok(items)
}

fn existing_turn(data: &[u8], props: &[Bx], associations: &[(bool, u16)]) -> Result<TransformOp, String> {
    let mut total = TransformOp::None;
    for (_, index) in associations {
        let Some(property) = usize::from(*index).checked_sub(1).and_then(|n| props.get(n)) else { continue };
        let value = uint(data, property.body, 1)?;
        let op = match &property.kind {
            b"irot" => [TransformOp::None, TransformOp::Rot270, TransformOp::Rot180, TransformOp::Rot90][(value & 3) as usize],
            // Axis 0 flips top to bottom and 1 left to right, as libheif and FFmpeg read it
            b"imir" if value & 1 == 0 => TransformOp::VFlip,
            b"imir" => TransformOp::HFlip,
            _ => continue,
        };
        total = then(total, op);
    }
    Ok(total)
}

fn grow(out: &mut [u8], b: &Bx, delta: i64) -> Result<(), String> {
    let Some((at, width)) = b.size_at else { return Ok(()) };
    let size = i64::try_from(uint(out, at, width)?).map_err(|_| broken())? + delta;
    let size = u64::try_from(size).map_err(|_| broken())?;
    if width == 4 {
        out[at..at + 4].copy_from_slice(&u32::try_from(size).map_err(|_| broken())?.to_be_bytes());
    } else {
        out[at..at + 8].copy_from_slice(&size.to_be_bytes());
    }
    Ok(())
}

/// Item data that sits after the changed header moves with it
fn move_item_data(out: &mut [u8], iloc: &Bx, from: usize, delta: i64) -> Result<(), String> {
    let version = *out.get(iloc.body).ok_or_else(broken)?;
    let sizes = uint(out, iloc.body + 4, 2)?;
    let (offset_width, length_width, base_width) = ((sizes >> 12 & 15) as usize, (sizes >> 8 & 15) as usize, (sizes >> 4 & 15) as usize);
    let index_width = if version >= 1 { (sizes & 15) as usize } else { 0 };
    if ![0, 4, 8].contains(&offset_width) || ![0, 4, 8].contains(&base_width) {
        return Err(broken());
    }
    let mut at = iloc.body + 6;
    let count = if version < 2 { uint(out, at, 2)? } else { uint(out, at, 4)? };
    at += if version < 2 { 2 } else { 4 };
    for _ in 0..count {
        at += if version < 2 { 2 } else { 4 };
        let method = if version >= 1 { uint(out, at, 2)? & 15 } else { 0 };
        if version >= 1 {
            at += 2;
        }
        let external = uint(out, at, 2)? != 0;
        at += 2;
        let base_at = at;
        let base = uint(out, at, base_width)?;
        at += base_width;
        let extents = uint(out, at, 2)?;
        at += 2;
        let mut places = Vec::new();
        for _ in 0..extents {
            at += index_width;
            places.push((at, uint(out, at, offset_width)?));
            at += offset_width + length_width;
        }
        if method != 0 || external {
            continue;
        }
        let moves = |value: u64| if value >= from as u64 { u64::try_from(i64::try_from(value).map_err(|_| broken())? + delta).map_err(|_| broken()) } else { Ok(value) };
        let mut patch = |at: usize, width: usize, value: u64| -> Result<(), String> {
            let bytes = value.to_be_bytes();
            if width == 4 && value > u64::from(u32::MAX) {
                return Err(broken());
            }
            out[at..at + width].copy_from_slice(&bytes[8 - width..]);
            Ok(())
        };
        if base_width > 0 && base >= from as u64 {
            patch(base_at, base_width, moves(base)?)?;
        } else if offset_width > 0 {
            for (place, offset) in places {
                patch(place, offset_width, moves(base + offset)? - base)?;
            }
        }
    }
    Ok(())
}

struct Layout<'a> {
    meta: &'a Bx,
    kids: Vec<Bx>,
    iprp: Bx,
    ipco: Bx,
    ipma: Bx,
    props: Vec<Bx>,
    version: u8,
    wide: bool,
    all: Vec<Entry>,
}

fn layout<'a>(data: &[u8], top: &'a [Bx]) -> Result<Layout<'a>, String> {
    let mut metas = top.iter().filter(|b| &b.kind == b"meta");
    let (Some(meta), None) = (metas.next(), metas.next()) else { return Err(broken()) };
    let kids = read_boxes(data, meta.body + 4, meta.end)?;
    let iprp = child(&kids, b"iprp")?;
    let properties = read_boxes(data, iprp.body, iprp.end)?;
    let mut ipmas = properties.iter().filter(|b| &b.kind == b"ipma");
    let (Some(ipma), None) = (ipmas.next(), ipmas.next()) else { return Err(broken()) };
    let ipco = child(&properties, b"ipco")?;
    let props = read_boxes(data, ipco.body, ipco.end)?;
    let version = *data.get(ipma.body).ok_or_else(broken)?;
    let (all, wide) = entries(data, ipma)?;
    let (ipco, ipma, iprp) = (ipco.clone(), ipma.clone(), iprp.clone());
    Ok(Layout { meta, kids, iprp, ipco, ipma, props, version, wide, all })
}

/// How the primary picture and its thumbnails are turned for showing now, by item number
#[cfg(test)]
pub fn turns(data: &[u8]) -> Result<Vec<(u64, TransformOp)>, String> {
    let top = read_boxes(data, 0, data.len())?;
    let l = layout(data, &top)?;
    let mut found = Vec::new();
    for item in targets(data, &l.kids)? {
        if let Some(entry) = l.all.iter().find(|e| e.item == item) {
            found.push((item, existing_turn(data, &l.props, &entry.associations)?));
        }
    }
    Ok(found)
}

/// The photo turned by `op` on top of whatever it already shows, as new bytes
pub fn turn(data: &[u8], op: TransformOp) -> Result<Vec<u8>, String> {
    if op == TransformOp::None {
        return Ok(data.to_vec());
    }
    let top = read_boxes(data, 0, data.len())?;
    if top.iter().any(|b| &b.kind == b"moov") {
        return Err("A HEIC sequence can't be turned without changing the picture".into());
    }
    let Layout { meta, kids, iprp, ipco, ipma, props, version, wide, all } = layout(data, &top)?;

    let mut added: Vec<[u8; 9]> = Vec::new();
    let mut add = |kind: &[u8; 4], value: u8| -> u16 {
        let mut b = [0, 0, 0, 9, 0, 0, 0, 0, value];
        b[4..8].copy_from_slice(kind);
        let place = added.iter().position(|a| *a == b).unwrap_or_else(|| {
            added.push(b);
            added.len() - 1
        });
        (props.len() + place + 1) as u16
    };
    let mut changes: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    for (n, item) in targets(data, &kids)?.into_iter().enumerate() {
        let Some(entry) = all.iter().find(|e| e.item == item) else {
            if n == 0 { return Err(broken()) } else { continue }
        };
        let total = then(existing_turn(data, &props, &entry.associations)?, op);
        let (quarters, mirror) = rotate_then_mirror(total);
        let mut associations: Vec<(bool, u16)> = entry.associations.iter().copied()
            .filter(|(_, index)| usize::from(*index).checked_sub(1).and_then(|n| props.get(n)).is_none_or(|p| &p.kind != b"irot" && &p.kind != b"imir"))
            .collect();
        if quarters > 0 {
            associations.push((true, add(b"irot", (4 - quarters) % 4)));
        }
        if mirror {
            associations.push((true, add(b"imir", 1)));
        }
        changes.push((entry.start, entry.end, encode(item, version, wide, &associations)?));
    }
    let new_properties: Vec<u8> = added.concat();
    let ipma_delta: i64 = changes.iter().map(|(start, end, bytes)| bytes.len() as i64 - (end - start) as i64).sum();
    let delta = ipma_delta + new_properties.len() as i64;

    let mut out = data.to_vec();
    if delta != 0 {
        let iloc = child(&kids, b"iloc")?;
        move_item_data(&mut out, iloc, meta.end, delta)?;
    }
    grow(&mut out, &ipma, ipma_delta)?;
    grow(&mut out, &ipco, new_properties.len() as i64)?;
    grow(&mut out, &iprp, delta)?;
    grow(&mut out, meta, delta)?;
    changes.push((ipco.end, ipco.end, new_properties));
    changes.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, bytes) in changes {
        out.splice(start..end, bytes);
    }
    Ok(out)
}
