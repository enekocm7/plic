//! How many exports may run at once.
//!
//! Stream copies barely touch the CPU and run alongside anything. Encodes
//! are capped by the user's setting, and further by what they encode on:
//! a hardware encoder has a handful of sessions, and a software encoder
//! already spreads one export over every core.

/// What an export keeps busy while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Workload {
    /// Packets copied as they are: disk bound.
    Copy,
    /// Encoded on the GPU's encoder.
    Hardware,
    /// Encoded on the CPU.
    Software,
}

/// Hardware encoders support several sessions (consumer NVENC cards five,
/// AMF and Quick Sync more), but two keep every one of them happy.
const HARDWARE_SESSIONS: usize = 2;
/// Copies are cheap but still read and write whole files.
const COPIES: usize = 4;
/// The most the user may ask for at once.
pub const MAX_ENCODES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Encodes of either kind at once, as the user set it.
    pub encodes: usize,
    pub hardware: usize,
    pub software: usize,
    pub copies: usize,
}

impl Limits {
    /// Limits for this machine with the user's `encodes` setting.
    pub fn for_machine(encodes: usize) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get());
        Self::new(encodes, cores)
    }

    fn new(encodes: usize, cores: usize) -> Self {
        Self {
            encodes: encodes.clamp(1, MAX_ENCODES),
            hardware: HARDWARE_SESSIONS,
            // x264 and friends scale well up to about four cores each.
            software: (cores / 4).clamp(1, MAX_ENCODES),
            copies: COPIES,
        }
    }

    /// Whether `next` may start while `running` are.
    pub fn allows(&self, running: &[Workload], next: Workload) -> bool {
        let count = |workload| running.iter().filter(|&&other| other == workload).count();
        match next {
            Workload::Copy => count(Workload::Copy) < self.copies,
            encode => {
                let encodes = running.len() - count(Workload::Copy);
                let cap = match encode {
                    Workload::Hardware => self.hardware,
                    _ => self.software,
                };
                encodes < self.encodes && count(encode) < cap
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Workload::*;

    #[test]
    fn one_at_a_time_still_lets_copies_through() {
        let limits = Limits::new(1, 16);
        assert!(limits.allows(&[], Hardware));
        assert!(!limits.allows(&[Hardware], Hardware));
        assert!(!limits.allows(&[Hardware], Software));
        assert!(limits.allows(&[Hardware, Copy], Copy));
        assert!(!limits.allows(&[Copy, Copy, Copy, Copy], Copy));
    }

    #[test]
    fn caps_each_kind_of_encoder() {
        let limits = Limits::new(4, 8);
        assert_eq!(limits.software, 2);
        assert!(limits.allows(&[Hardware], Hardware));
        assert!(!limits.allows(&[Hardware, Hardware], Hardware));
        assert!(limits.allows(&[Hardware, Hardware, Software], Software));
        assert!(!limits.allows(&[Hardware, Software, Software], Software));
        // Copies never count against encodes.
        assert!(limits.allows(&[Copy, Copy, Copy], Hardware));
    }

    #[test]
    fn small_machines_encode_one_in_software() {
        let limits = Limits::new(3, 2);
        assert_eq!(limits.software, 1);
        assert!(!limits.allows(&[Software], Software));
        assert!(limits.allows(&[Software], Hardware));
        assert_eq!(Limits::new(9, 64).encodes, MAX_ENCODES);
    }
}
