//! Gives a photo an orientation tag when it has none, without moving the pictures behind the first one (an Ultra HDR gain map) away from where the file says they are.

use super::app_segments;

fn read16(data: &[u8], at: usize, le: bool) -> Option<u16> {
    let bytes = data.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(if le { u16::from_le_bytes(bytes) } else { u16::from_be_bytes(bytes) })
}

fn read32(data: &[u8], at: usize, le: bool) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(if le { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) })
}

fn write32(data: &mut [u8], at: usize, value: u32, le: bool) {
    data[at..at + 4].copy_from_slice(&if le { value.to_le_bytes() } else { value.to_be_bytes() });
}

/// Whether the TIFF data is little endian, if it starts like TIFF data
fn little_endian(tiff: &[u8]) -> Option<bool> {
    let le = match tiff.get(..2)? { b"II" => true, b"MM" => false, _ => return None };
    (read16(tiff, 2, le)? == 0x2a && tiff.len() >= 8).then_some(le)
}

/// An IFD entry for the orientation: a short, one value, which sits in the entry itself
fn orientation_entry(value: u16, le: bool) -> Vec<u8> {
    let short = |n: u16| if le { n.to_le_bytes() } else { n.to_be_bytes() };
    let count = if le { 1u32.to_le_bytes() } else { 1u32.to_be_bytes() };
    [short(0x0112).as_slice(), &short(3), &count, &short(value), &[0, 0]].concat()
}

/// Whether an IFD, its entries and everything they point to lie before `limit`, following the pointers to the EXIF, GPS and interoperability IFDs
fn ends_before(tiff: &[u8], le: bool, ifd: usize, limit: usize, depth: u8) -> Option<bool> {
    let count = usize::from(read16(tiff, ifd, le)?);
    let entries_end = ifd.checked_add(2)?.checked_add(count * 12)?;
    if entries_end.checked_add(4)? > limit { return Some(false); }
    for index in 0..count {
        let at = ifd + 2 + index * 12;
        let (tag, kind, n) = (read16(tiff, at, le)?, read16(tiff, at + 2, le)?, usize::try_from(read32(tiff, at + 4, le)?).ok()?);
        let size = n.checked_mul(match kind { 1 | 2 | 6 | 7 => 1, 3 | 8 => 2, 4 | 9 | 11 | 13 => 4, 5 | 10 | 12 => 8, _ => return None })?;
        if size > 4 {
            let offset = usize::try_from(read32(tiff, at + 8, le)?).ok()?;
            if offset.checked_add(size)? > limit { return Some(false); }
        }
        if matches!(tag, 0x8769 | 0x8825 | 0xa005) {
            if depth == 0 { return None; }
            let sub = usize::try_from(read32(tiff, at + 8, le)?).ok()?;
            if !ends_before(tiff, le, sub, limit, depth - 1)? { return Some(false); }
        }
    }
    Some(true)
}

/// Where the thumbnail's IFD starts, if that and the thumbnail are the very end of the TIFF data and nothing else in it points there, so cutting the data off at that point loses nothing the file still uses
fn thumbnail_start(tiff: &[u8], le: bool, ifd: usize, entries_end: usize) -> Option<usize> {
    let next = usize::try_from(read32(tiff, entries_end, le)?).ok()?;
    if next == 0 { return None; }
    let count = usize::from(read16(tiff, next, le)?);
    let (mut offset, mut length) = (None, None);
    for index in 0..count {
        let at = next.checked_add(2 + index * 12)?;
        match read16(tiff, at, le)? {
            0x0201 => offset = usize::try_from(read32(tiff, at + 8, le)?).ok(),
            0x0202 => length = usize::try_from(read32(tiff, at + 8, le)?).ok(),
            _ => {}
        }
    }
    let ifd_end = next.checked_add(2 + count * 12 + 4)?;
    let (offset, length) = (offset?, length?);
    if offset < ifd_end || offset.checked_add(length)? < tiff.len().saturating_sub(1) || offset + length > tiff.len() { return None; }
    ends_before(tiff, le, ifd, next, 2)?.then_some(next)
}

/// The TIFF data with a new first IFD behind it that has everything the old one had plus the orientation. Nothing before it moves, so every offset in the old data stays right. The old IFD stays as unused bytes, and the new one links to no thumbnail.
fn extended_tiff(tiff: &[u8], value: u16, trim: bool) -> Option<Vec<u8>> {
    let le = little_endian(tiff)?;
    let ifd = usize::try_from(read32(tiff, 4, le)?).ok()?;
    let count = usize::from(read16(tiff, ifd, le)?);
    let entries_at = ifd.checked_add(2)?;
    let entries_end = entries_at.checked_add(count * 12)?;
    let entries = tiff.get(entries_at..entries_end)?;
    tiff.get(entries_end..entries_end.checked_add(4)?)?;
    let tags: Vec<u16> = entries.chunks(12).map(|e| read16(e, 0, le)).collect::<Option<_>>()?;
    if tags.contains(&0x0112) { return None; }
    let place = tags.iter().position(|tag| *tag > 0x0112).unwrap_or(count);
    let mut out = tiff.to_vec();
    if trim { out.truncate(thumbnail_start(tiff, le, ifd, entries_end)?); }
    if out.len() % 2 == 1 { out.push(0); }
    let new_ifd = u32::try_from(out.len()).ok()?;
    let new_count = u16::try_from(count + 1).ok()?;
    out.extend(if le { new_count.to_le_bytes() } else { new_count.to_be_bytes() });
    out.extend(&entries[..place * 12]);
    out.extend(orientation_entry(value, le));
    out.extend(&entries[place * 12..]);
    out.extend([0; 4]);
    write32(&mut out, 4, new_ifd, le);
    Some(out)
}

/// A whole EXIF segment around the TIFF data, if it fits in the 64 KiB a segment can hold
fn exif_segment(tiff: &[u8]) -> Option<Vec<u8>> {
    let length = u16::try_from(tiff.len() + 8).ok()?;
    Some([&[0xff, 0xe1][..], &length.to_be_bytes(), b"Exif\0\0", tiff].concat())
}

/// An EXIF segment that holds nothing but the orientation
fn minimal_exif(value: u16) -> Option<Vec<u8>> {
    let tiff = [b"MM\0\x2a\0\0\0\x08\0\x01".as_slice(), &orientation_entry(value, false), &[0; 4]].concat();
    exif_segment(&tiff)
}

/// Where the list of pictures in the file sits: the entries of its MP Index IFD, which give each picture a size and an offset from the start of the MPF TIFF data
struct Mpf {
    tiff: usize,
    le: bool,
    entries: usize,
    count: usize,
}

const UNFOLLOWABLE: &str = "The photo says where its other pictures are in a way that can't be followed";

fn find_mpf(data: &[u8]) -> Result<Option<Mpf>, String> {
    let Some((_, start, end)) = app_segments(data).into_iter().find(|(marker, start, end)| *marker == 0xe2 && data[*start..*end].starts_with(b"MPF\0")) else { return Ok(None) };
    let tiff = start + 4;
    let block = &data[tiff..end];
    let found = (|| {
        let le = little_endian(block)?;
        let ifd = usize::try_from(read32(block, 4, le)?).ok()?;
        for index in 0..usize::from(read16(block, ifd, le)?) {
            let at = ifd.checked_add(2 + index * 12)?;
            if read16(block, at, le)? != 0xb002 { continue; }
            let size = usize::try_from(read32(block, at + 4, le)?).ok()?;
            let offset = usize::try_from(read32(block, at + 8, le)?).ok()?;
            if read16(block, at + 2, le)? != 7 || size == 0 || size % 16 != 0 { return None; }
            block.get(offset..offset.checked_add(size)?)?;
            return Some(Mpf { tiff, le, entries: tiff + offset, count: size / 16 });
        }
        None
    })();
    found.map(Some).ok_or_else(|| UNFOLLOWABLE.to_string())
}

/// Every other picture has to start with a JPEG header where the list says, or the list can't be trusted to be fixed
fn check_pictures(data: &[u8], mpf: &Mpf) -> Option<()> {
    for index in 1..mpf.count {
        let offset = read32(data, mpf.entries + index * 16 + 8, mpf.le)?;
        if offset == 0 { continue; }
        if !data.get(mpf.tiff.checked_add(usize::try_from(offset).ok()?)?..)?.starts_with(&[0xff, 0xd8, 0xff]) { return None; }
    }
    Some(())
}

/// Fixes the list after `delta` bytes were added (or taken away, if negative) in the first picture's headers where `grew_at` was. The first picture is that much bigger. The other pictures all moved by `delta`, and so did the list if it comes after `grew_at`: then their offsets from it are the same, and if it comes before, they are `delta` further away.
fn fix_mpf(out: &mut [u8], mpf: &Mpf, delta: i64, grew_at: usize) -> Option<()> {
    let moved = |value: usize| usize::try_from(i64::try_from(value).ok()?.checked_add(delta)?).ok();
    let grown = |value: u32| u32::try_from(i64::from(value).checked_add(delta)?).ok();
    let list_moved = grew_at <= mpf.tiff;
    let (tiff, entries) = if list_moved { (moved(mpf.tiff)?, moved(mpf.entries)?) } else { (mpf.tiff, mpf.entries) };
    for index in 0..mpf.count {
        let at = entries + index * 16;
        let size = read32(out, at + 4, mpf.le)?;
        let offset = read32(out, at + 8, mpf.le)?;
        if index == 0 {
            if size != 0 { write32(out, at + 4, grown(size)?, mpf.le); }
        } else if offset != 0 {
            let offset = if list_moved { offset } else { grown(offset)? };
            write32(out, at + 8, offset, mpf.le);
            if !out.get(tiff.checked_add(usize::try_from(offset).ok()?)?..)?.starts_with(&[0xff, 0xd8, 0xff]) { return None; }
        }
    }
    Some(())
}

/// The photo with the orientation `value` added as a tag, in its EXIF segment (extended if it has one, or a new one after any JFIF header if not) with the list of pictures in an Ultra HDR photo fixed to match. The gain map's bytes are never touched.
pub fn add_orientation(data: &[u8], value: u16) -> Result<Vec<u8>, String> {
    let mpf = find_mpf(data)?;
    if let Some(mpf) = &mpf { check_pictures(data, mpf).ok_or(UNFOLLOWABLE)?; }
    let segments = app_segments(data);
    let no_room = || "The photo's EXIF data has no room for an orientation tag".to_string();
    // The bytes from `from` up to `to` are replaced by `segment`
    let (from, to, segment) = match segments.iter().find(|(marker, start, end)| *marker == 0xe1 && data[*start..*end].starts_with(b"Exif\0\0")) {
        Some(&(_, start, end)) => (start - 4, end, {
            let old = &data[start + 6..end];
            // Cutting the thumbnail off is the fallback when keeping its bytes would not fit
            extended_tiff(old, value, false).and_then(|tiff| exif_segment(&tiff))
                .or_else(|| extended_tiff(old, value, true).and_then(|tiff| exif_segment(&tiff)))
                .ok_or_else(no_room)?
        }),
        None => {
            let mut at = 2;
            for &(marker, start, end) in &segments {
                if marker != 0xe0 || start - 4 != at { break; }
                at = end;
            }
            (at, at, minimal_exif(value).ok_or_else(no_room)?)
        }
    };
    let mut out = [&data[..from], &segment, &data[to..]].concat();
    let delta = out.len() as i64 - data.len() as i64;
    if let Some(mpf) = &mpf { fix_mpf(&mut out, mpf, delta, to).ok_or(UNFOLLOWABLE)?; }
    let read_back = super::exif_places(&out).into_iter().find_map(|(start, end)| super::patch_orientation(&mut out[start..end].to_vec(), |v| v, false));
    if read_back != Some(value) { return Err(no_room()); }
    Ok(out)
}
