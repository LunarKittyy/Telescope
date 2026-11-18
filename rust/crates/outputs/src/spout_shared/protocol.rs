//! Spout2's shared-memory layout and object names (SpoutSenderNames.h, SpoutSharedMemory.cpp, SpoutFrameCount.cpp), as plain data
//! so it can be checked on any system.

/// The file mapping that lists the senders.
pub const NAMES_MAP: &str = "SpoutSenderNames";
/// The file mapping that holds the name of the sender receivers take when asked for "the active sender" (one name, NAME_LEN bytes).
pub const ACTIVE_MAP: &str = "ActiveSenderName";
/// Bytes each sender name takes in that list, null-terminated; the list ends at the first empty name.
pub const NAME_LEN: usize = 256;

/// The mutex that guards the file mapping `map`.
pub fn map_mutex(map: &str) -> String {
    format!("{map}_mutex")
}

/// The semaphore a sender counts its frames on (only while frame counting is on for the sender).
pub fn count_semaphore(sender: &str) -> String {
    format!("{sender}_Count_Semaphore")
}

/// The named mutex that guards a sender's texture when the texture has no keyed mutex of its own.
pub fn access_mutex(sender: &str) -> String {
    format!("{sender}_SpoutAccessMutex")
}

/// What a sender writes into the file mapping named after it (SharedTextureInfo, 280 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedTextureInfo {
    /// The texture's D3D11 share handle, which fits 32 bits whichever way the process is built.
    pub share_handle: u32,
    pub width: u32,
    pub height: u32,
    /// A DXGI format number, 0 when the sender doesn't say (B8G8R8A8).
    pub format: u32,
    pub usage: u32,
    /// The sender's program path as bytes, null-padded.
    pub description: [u8; 256],
    /// Top bits say the sender shares through the CPU (0x8000_0000) or is OpenGL/DirectX compatible (0x4000_0000).
    pub partner_id: u32,
}

impl SharedTextureInfo {
    pub const SIZE: usize = 280;

    /// From the first 280 bytes of a sender's mapping; None if there are fewer.
    pub fn parse(bytes: &[u8]) -> Option<SharedTextureInfo> {
        let bytes = bytes.get(..Self::SIZE)?;
        let word = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let mut description = [0u8; 256];
        description.copy_from_slice(&bytes[20..276]);
        Some(SharedTextureInfo {
            share_handle: word(0), width: word(4), height: word(8), format: word(12), usage: word(16), description, partner_id: word(276),
        })
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        for (i, v) in [self.share_handle, self.width, self.height, self.format, self.usage].iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        out[20..276].copy_from_slice(&self.description);
        out[276..].copy_from_slice(&self.partner_id.to_le_bytes());
        out
    }
}

/// The names in the sender list's bytes, in list order. A name that isn't UTF-8 is left out, since it can't be named again exactly.
pub fn parse_names(list: &[u8]) -> Vec<String> {
    list.chunks(NAME_LEN)
        .map(|entry| &entry[..entry.iter().position(|b| *b == 0).unwrap_or(entry.len())])
        .take_while(|name| !name.is_empty())
        .filter_map(|name| std::str::from_utf8(name).ok().map(str::to_string))
        .collect()
}

/// How the bytes of a pixel run, for the 8-bit formats a sender's texture can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelOrder {
    Bgra,
    /// As Bgra with the fourth byte unused.
    Bgrx,
    Rgba,
}

impl ChannelOrder {
    /// Of a DXGI format number (87 B8G8R8A8, 88 B8G8R8X8, 28 R8G8B8A8, with their sRGB and typeless kin), 0 counting as B8G8R8A8; None for anything else.
    pub fn of_dxgi(format: u32) -> Option<ChannelOrder> {
        match format {
            0 | 87 | 90 | 91 => Some(ChannelOrder::Bgra),
            88 | 92 | 93 => Some(ChannelOrder::Bgrx),
            27..=29 => Some(ChannelOrder::Rgba),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sender_info_reads_back_as_written() {
        let mut description = [0u8; 256];
        description[..4].copy_from_slice(b"C:\\a");
        let info = SharedTextureInfo { share_handle: 0xC000_0123, width: 1920, height: 1080, format: 87, usage: 0, description, partner_id: 0x4000_0000 };
        let bytes = info.to_bytes();
        assert_eq!(bytes.len(), 280);
        assert_eq!(&bytes[..8], &[0x23, 0x01, 0x00, 0xC0, 0x80, 0x07, 0x00, 0x00]);
        assert_eq!(SharedTextureInfo::parse(&bytes), Some(info));
        assert_eq!(SharedTextureInfo::parse(&bytes[..279]), None);
    }

    #[test]
    fn the_sender_list_ends_at_the_first_empty_name() {
        let mut list = vec![0u8; 4 * NAME_LEN];
        list[..5].copy_from_slice(b"Cam 1");
        list[NAME_LEN..NAME_LEN + 3].copy_from_slice(b"OBS");
        list[3 * NAME_LEN..3 * NAME_LEN + 4].copy_from_slice(b"Late"); // after the end: never listed
        assert_eq!(parse_names(&list), vec!["Cam 1", "OBS"]);
        assert!(parse_names(&[0u8; 512]).is_empty());
        // A name filling its 256 bytes has no null
        let full = vec![b'a'; NAME_LEN];
        assert_eq!(parse_names(&full), vec!["a".repeat(NAME_LEN)]);
    }

    #[test]
    fn a_name_that_isnt_utf8_is_left_out() {
        let mut list = vec![0u8; 2 * NAME_LEN];
        list[..3].copy_from_slice(&[0xE9, b'a', b'b']);
        list[NAME_LEN..NAME_LEN + 2].copy_from_slice(b"ok");
        assert_eq!(parse_names(&list), vec!["ok"]);
    }

    #[test]
    fn object_names_follow_spout() {
        assert_eq!(map_mutex("SpoutSenderNames"), "SpoutSenderNames_mutex");
        assert_eq!(count_semaphore("Cam"), "Cam_Count_Semaphore");
        assert_eq!(access_mutex("Cam"), "Cam_SpoutAccessMutex");
    }

    #[test]
    fn dxgi_formats_map_to_channel_orders() {
        assert_eq!(ChannelOrder::of_dxgi(0), Some(ChannelOrder::Bgra));
        assert_eq!(ChannelOrder::of_dxgi(87), Some(ChannelOrder::Bgra));
        assert_eq!(ChannelOrder::of_dxgi(88), Some(ChannelOrder::Bgrx));
        assert_eq!(ChannelOrder::of_dxgi(28), Some(ChannelOrder::Rgba));
        assert_eq!(ChannelOrder::of_dxgi(10), None); // R16G16B16A16_FLOAT
    }
}
