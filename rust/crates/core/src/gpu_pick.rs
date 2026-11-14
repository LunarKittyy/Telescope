//! Which graphics card to try first, as a plain ordering the media and shell crates both feed from wgpu's adapter list (core never names wgpu).

/// The graphics API an adapter came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    Vulkan,
    Dx12,
    Metal,
    Gl,
}

/// What kind of device the adapter says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Integrated,
    Discrete,
    Other,
    Virtual,
    Cpu,
}

/// One adapter as the enumeration listed it: its API, its kind, and its place in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub api: Api,
    pub kind: Kind,
    pub index: usize,
}

/// Indices (`Candidate::index`) in the order to try them: real GPUs first, then Other, Virtual, Cpu. Inside a group Vulkan goes first
/// on every OS (every Windows install so far ran on it), then DX12, Metal and GL, and inside an API the driver's own order stays,
/// since that is what the OS's choice shows up in.
pub fn order(candidates: &[Candidate]) -> Vec<usize> {
    let tier = |k: Kind| match k {
        Kind::Integrated | Kind::Discrete => 0,
        Kind::Other => 1,
        Kind::Virtual => 2,
        Kind::Cpu => 3,
    };
    let api = |a: Api| match a {
        Api::Vulkan => 0,
        Api::Dx12 => 1,
        Api::Metal => 2,
        Api::Gl => 3,
    };
    let mut sorted = candidates.to_vec();
    sorted.sort_by_key(|c| (tier(c.kind), api(c.api), c.index));
    sorted.into_iter().map(|c| c.index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(api: Api, kind: Kind, index: usize) -> Candidate {
        Candidate { api, kind, index }
    }

    #[test]
    fn real_gpus_come_before_software_and_odd_ones() {
        let list = [c(Api::Vulkan, Kind::Cpu, 0), c(Api::Vulkan, Kind::Other, 1), c(Api::Vulkan, Kind::Virtual, 2), c(Api::Vulkan, Kind::Integrated, 3)];
        assert_eq!(order(&list), [3, 1, 2, 0]);
    }

    #[test]
    fn the_drivers_order_among_real_gpus_stays() {
        let list = [c(Api::Vulkan, Kind::Integrated, 0), c(Api::Vulkan, Kind::Discrete, 1)];
        assert_eq!(order(&list), [0, 1]);
        let list = [c(Api::Vulkan, Kind::Discrete, 0), c(Api::Vulkan, Kind::Integrated, 1)];
        assert_eq!(order(&list), [0, 1]);
    }

    #[test]
    fn vulkan_goes_before_dx12() {
        let list = [c(Api::Dx12, Kind::Integrated, 0), c(Api::Dx12, Kind::Discrete, 1), c(Api::Vulkan, Kind::Discrete, 2)];
        assert_eq!(order(&list), [2, 0, 1]);
    }

    #[test]
    fn a_real_vulkan_gpu_beats_a_dx12_software_adapter() {
        let list = [c(Api::Dx12, Kind::Cpu, 0), c(Api::Vulkan, Kind::Integrated, 1)];
        assert_eq!(order(&list), [1, 0]);
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(order(&[]).is_empty());
    }
}
