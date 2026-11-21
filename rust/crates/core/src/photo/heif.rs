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

/// A number that has to sit before `end`, the end of the box it is read from
fn field(data: &[u8], at: usize, width: usize, end: usize) -> Result<u64, String> {
    if at.checked_add(width).is_none_or(|stop| stop > end) {
        return Err(broken());
    }
    uint(data, at, width)
}

/// The box that starts at `at` inside the bytes up to `to`. Size 0 runs to the end of them, size 1 has the size written after the name.
fn header(data: &[u8], at: usize, to: usize) -> Result<Bx, String> {
    let size = field(data, at, 4, to)?;
    if at.checked_add(8).is_none_or(|stop| stop > to) {
        return Err(broken());
    }
    let kind: [u8; 4] = data.get(at + 4..at + 8).and_then(|k| k.try_into().ok()).ok_or_else(broken)?;
    let (body, end, size_at) = match size {
        0 => (at + 8, to, None),
        1 => {
            let end = usize::try_from(field(data, at + 8, 8, to)?).ok().and_then(|n| at.checked_add(n)).ok_or_else(broken)?;
            (at + 16, end, Some((at + 8, 8)))
        }
        n => (at + 8, at.checked_add(usize::try_from(n).map_err(|_| broken())?).ok_or_else(broken)?, Some((at, 4))),
    };
    if end > to || end < body {
        return Err(broken());
    }
    Ok(Bx { body, end, kind, size_at })
}

fn read_boxes(data: &[u8], from: usize, to: usize) -> Result<Vec<Bx>, String> {
    let mut found = Vec::new();
    let mut at = from;
    while at < to {
        let b = header(data, at, to)?;
        at = b.end;
        found.push(b);
    }
    Ok(found)
}

fn child<'a>(kids: &'a [Bx], kind: &[u8; 4]) -> Result<&'a Bx, String> {
    kids.iter().find(|b| &b.kind == kind).ok_or_else(broken)
}

/// The version of a box that has one, and where what comes after the version and flags starts
fn full_box(data: &[u8], b: &Bx) -> Result<(u8, usize), String> {
    Ok(((field(data, b.body, 4, b.end)? >> 24) as u8, b.body + 4))
}

pub fn is_heif(data: &[u8]) -> bool {
    let Ok(first) = header(data, 0, data.len()) else { return false };
    if &first.kind != b"ftyp" {
        return false;
    }
    let known = |brand: &[u8]| matches!(brand, b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1");
    let brands = &data[first.body..first.end];
    brands.get(..4).is_some_and(known) || brands.get(8..).unwrap_or_default().chunks(4).any(known)
}

struct Entry {
    start: usize,
    end: usize,
    item: u64,
    associations: Vec<(bool, u16)>,
}

/// One `ipma` box. A file can have several, each with its own item numbers.
struct Table {
    bx: Bx,
    version: u8,
    wide: bool,
    entries: Vec<Entry>,
}

fn table(data: &[u8], ipma: &Bx) -> Result<Table, String> {
    let (version, mut at) = full_box(data, ipma)?;
    let wide = field(data, ipma.body + 1, 3, ipma.end)? & 1 == 1;
    let count = field(data, at, 4, ipma.end)?;
    at += 4;
    let mut entries = Vec::new();
    for _ in 0..count {
        let start = at;
        let id_width = if version == 0 { 2 } else { 4 };
        let item = field(data, at, id_width, ipma.end)?;
        let many = field(data, at + id_width, 1, ipma.end)?;
        at += id_width + 1;
        let mut associations = Vec::new();
        for _ in 0..many {
            let (essential, index) = if wide {
                let v = field(data, at, 2, ipma.end)? as u16;
                at += 2;
                (v >> 15 == 1, v & 0x7fff)
            } else {
                let v = field(data, at, 1, ipma.end)? as u16;
                at += 1;
                (v >> 7 == 1, v & 0x7f)
            };
            associations.push((essential, index));
        }
        entries.push(Entry { start, end: at, item, associations });
    }
    Ok(Table { bx: ipma.clone(), version, wide, entries })
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

struct Reference {
    kind: [u8; 4],
    from: u64,
    to: Vec<u64>,
}

/// What the `iref` box says about how items belong together
fn references(data: &[u8], meta: &[Bx]) -> Result<Vec<Reference>, String> {
    let Some(iref) = meta.iter().find(|b| &b.kind == b"iref") else { return Ok(Vec::new()) };
    let (version, start) = full_box(data, iref)?;
    let width = if version == 0 { 2 } else { 4 };
    let mut found = Vec::new();
    for reference in read_boxes(data, start, iref.end)? {
        let from = field(data, reference.body, width, reference.end)?;
        let count = field(data, reference.body + width, 2, reference.end)? as usize;
        let at = reference.body + width + 2;
        let to = (0..count).map(|n| field(data, at + n * width, width, reference.end)).collect::<Result<Vec<_>, _>>()?;
        found.push(Reference { kind: reference.kind, from, to });
    }
    Ok(found)
}

/// The primary picture and the thumbnails that point at it
fn targets(data: &[u8], meta: &[Bx], references: &[Reference]) -> Result<Vec<u64>, String> {
    let pitm = child(meta, b"pitm")?;
    let (version, at) = full_box(data, pitm)?;
    let primary = field(data, at, if version == 0 { 2 } else { 4 }, pitm.end)?;
    let mut items = vec![primary];
    items.extend(references.iter().filter(|r| &r.kind == b"thmb" && r.to.contains(&primary)).map(|r| r.from));
    Ok(items)
}

fn property(props: &[Bx], index: u16) -> Option<&Bx> {
    usize::from(index).checked_sub(1).and_then(|n| props.get(n))
}

fn existing_turn(data: &[u8], props: &[Bx], associations: &[(bool, u16)]) -> Result<TransformOp, String> {
    let mut total = TransformOp::None;
    for (_, index) in associations {
        let Some(property) = property(props, *index) else { continue };
        let op = match &property.kind {
            b"irot" => [TransformOp::None, TransformOp::Rot270, TransformOp::Rot180, TransformOp::Rot90][(field(data, property.body, 1, property.end)? & 3) as usize],
            // Axis 0 flips top to bottom and 1 left to right, as libheif and FFmpeg read it
            b"imir" if field(data, property.body, 1, property.end)? & 1 == 0 => TransformOp::VFlip,
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

/// A part of the file that is replaced: where it starts and ends, and how long its replacement is
type Splice = (usize, usize, usize);

/// Where a place in the file ends up once the splices are made. A place inside a replaced part has nowhere to go.
fn moved(place: u64, splices: &[Splice]) -> Result<u64, String> {
    let mut shift = 0i128;
    for &(start, end, length) in splices {
        if place >= end as u64 {
            shift += length as i128 - (end - start) as i128;
        } else if place > start as u64 {
            return Err(broken());
        }
    }
    u64::try_from(i128::from(place) + shift).map_err(|_| broken())
}

/// Item data that sits after a changed part moves with it, wherever the data is: after the `meta` box or inside it
fn move_item_data(out: &mut [u8], iloc: &Bx, splices: &[Splice]) -> Result<(), String> {
    let (version, mut at) = full_box(out, iloc)?;
    if version > 2 {
        return Err(broken());
    }
    let sizes = field(out, at, 2, iloc.end)?;
    at += 2;
    let (offset_width, length_width, base_width) = ((sizes >> 12 & 15) as usize, (sizes >> 8 & 15) as usize, (sizes >> 4 & 15) as usize);
    let index_width = if version >= 1 { (sizes & 15) as usize } else { 0 };
    if [offset_width, length_width, base_width, index_width].iter().any(|w| ![0, 4, 8].contains(w)) {
        return Err(broken());
    }
    let id_width = if version < 2 { 2 } else { 4 };
    let count = field(out, at, id_width, iloc.end)?;
    at += id_width;
    for _ in 0..count {
        at += id_width;
        let method = if version >= 1 { field(out, at, 2, iloc.end)? & 15 } else { 0 };
        if version >= 1 {
            at += 2;
        }
        let external = field(out, at, 2, iloc.end)? != 0;
        at += 2;
        let base_at = at;
        let base = field(out, at, base_width, iloc.end)?;
        at += base_width;
        let extents = field(out, at, 2, iloc.end)?;
        at += 2;
        let mut places = Vec::new();
        for _ in 0..extents {
            at += index_width;
            places.push((at, field(out, at, offset_width, iloc.end)?));
            at += offset_width + length_width;
        }
        if at > iloc.end {
            return Err(broken());
        }
        // Method 1 counts from the start of `idat`, which moves as a whole, and the rest is not in this file
        if method != 0 || external {
            continue;
        }
        let mut shifts = Vec::new();
        for (_, offset) in &places {
            let place = base.checked_add(*offset).ok_or_else(broken)?;
            shifts.push(i128::from(moved(place, splices)?) - i128::from(place));
        }
        if shifts.iter().all(|shift| *shift == 0) {
            continue;
        }
        let mut patch = |at: usize, width: usize, value: u64, shift: i128| -> Result<(), String> {
            let value = u64::try_from(i128::from(value) + shift).map_err(|_| broken())?;
            if width == 4 && value > u64::from(u32::MAX) {
                return Err(broken());
            }
            out[at..at + width].copy_from_slice(&value.to_be_bytes()[8 - width..]);
            Ok(())
        };
        if base_width > 0 && shifts.iter().all(|shift| *shift == shifts[0]) {
            patch(base_at, base_width, base, shifts[0])?;
        } else if offset_width > 0 {
            for ((place, offset), shift) in places.into_iter().zip(shifts) {
                patch(place, offset_width, offset, shift)?;
            }
        } else {
            return Err(broken());
        }
    }
    Ok(())
}

struct Layout<'a> {
    meta: &'a Bx,
    kids: Vec<Bx>,
    iprp: Bx,
    ipco: Bx,
    tables: Vec<Table>,
    props: Vec<Bx>,
}

fn layout<'a>(data: &[u8], top: &'a [Bx]) -> Result<Layout<'a>, String> {
    let mut metas = top.iter().filter(|b| &b.kind == b"meta");
    let (Some(meta), None) = (metas.next(), metas.next()) else { return Err(broken()) };
    let kids = read_boxes(data, full_box(data, meta)?.1, meta.end)?;
    let iprp = child(&kids, b"iprp")?;
    let properties = read_boxes(data, iprp.body, iprp.end)?;
    let tables = properties.iter().filter(|b| &b.kind == b"ipma").map(|b| table(data, b)).collect::<Result<Vec<_>, _>>()?;
    if tables.is_empty() {
        return Err(broken());
    }
    let ipco = child(&properties, b"ipco")?;
    let props = read_boxes(data, ipco.body, ipco.end)?;
    let (ipco, iprp) = (ipco.clone(), iprp.clone());
    Ok(Layout { meta, kids, iprp, ipco, tables, props })
}

/// How the primary picture and its thumbnails are turned for showing now, by item number
#[cfg(test)]
pub fn turns(data: &[u8]) -> Result<Vec<(u64, TransformOp)>, String> {
    let top = read_boxes(data, 0, data.len())?;
    let l = layout(data, &top)?;
    let mut found = Vec::new();
    for item in targets(data, &l.kids, &references(data, &l.kids)?)? {
        if let Some(entry) = l.tables.iter().flat_map(|t| &t.entries).find(|e| e.item == item) {
            found.push((item, existing_turn(data, &l.props, &entry.associations)?));
        }
    }
    Ok(found)
}

/// The photo turned by `op` on top of whatever it already shows, as new bytes. A photo that can't be turned exactly is refused.
pub fn turn(data: &[u8], op: TransformOp) -> Result<Vec<u8>, String> {
    if op == TransformOp::None {
        return Ok(data.to_vec());
    }
    let top = read_boxes(data, 0, data.len())?;
    if top.iter().any(|b| &b.kind == b"moov") {
        return Err("A HEIC sequence can't be turned without changing the picture".into());
    }
    let Layout { meta, kids, iprp, ipco, tables, props } = layout(data, &top)?;
    let references = references(data, &kids)?;
    let items = targets(data, &kids, &references)?;
    // Alpha, depth and tiles are laid over or cut from the picture as it is coded, so turning only the picture would put them out of line
    if references.iter().any(|r| (&r.kind == b"auxl" || &r.kind == b"dimg") && r.to.iter().any(|to| items.contains(to))) {
        return Err("A HEIC photo with alpha, depth or tiles made from its picture can't be turned without changing them".into());
    }

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
    let mut table_deltas = vec![0i64; tables.len()];
    for (n, item) in items.into_iter().enumerate() {
        let Some((at, entry)) = tables.iter().enumerate().find_map(|(at, t)| t.entries.iter().find(|e| e.item == item).map(|e| (at, e))) else {
            if n == 0 { return Err(broken()) } else { continue }
        };
        // A crop or a scaling is done in the order the properties come in, and this only knows how to put a turn after the rest
        if entry.associations.iter().any(|(_, index)| property(&props, *index).is_some_and(|p| &p.kind == b"clap" || &p.kind == b"iscl")) {
            return Err("A HEIC photo that is cropped or scaled by its properties can't be turned without changing the picture".into());
        }
        let total = then(existing_turn(data, &props, &entry.associations)?, op);
        let (quarters, mirror) = rotate_then_mirror(total);
        let mut associations: Vec<(bool, u16)> = entry.associations.iter().copied()
            .filter(|(_, index)| property(&props, *index).is_none_or(|p| &p.kind != b"irot" && &p.kind != b"imir"))
            .collect();
        if quarters > 0 {
            associations.push((true, add(b"irot", (4 - quarters) % 4)));
        }
        if mirror {
            associations.push((true, add(b"imir", 1)));
        }
        let bytes = encode(item, tables[at].version, tables[at].wide, &associations)?;
        table_deltas[at] += bytes.len() as i64 - (entry.end - entry.start) as i64;
        changes.push((entry.start, entry.end, bytes));
    }
    let new_properties: Vec<u8> = added.concat();
    let delta = table_deltas.iter().sum::<i64>() + new_properties.len() as i64;
    changes.push((ipco.end, ipco.end, new_properties.clone()));
    let splices: Vec<Splice> = changes.iter().map(|(start, end, bytes)| (*start, *end, bytes.len())).collect();

    let mut out = data.to_vec();
    if delta != 0 {
        move_item_data(&mut out, child(&kids, b"iloc")?, &splices)?;
    }
    for (table, table_delta) in tables.iter().zip(&table_deltas) {
        grow(&mut out, &table.bx, *table_delta)?;
    }
    grow(&mut out, &ipco, new_properties.len() as i64)?;
    grow(&mut out, &iprp, delta)?;
    grow(&mut out, meta, delta)?;
    changes.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, bytes) in changes {
        out.splice(start..end, bytes);
    }
    Ok(out)
}
