/// Control commands for HLS worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HlsCommand {
    /// Seek to a specific segment.
    Seek {
        /// Target segment index.
        segment_index: usize,
        /// New epoch (invalidates stale chunks).
        epoch: u64,
    },

    /// Force switch to a specific variant (manual ABR override).
    ForceVariant {
        /// Target variant index.
        variant_index: usize,
        /// New epoch.
        epoch: u64,
    },

    /// Pause chunk emission (worker continues but doesn't yield chunks).
    Pause,

    /// Resume chunk emission after pause.
    Resume,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_creation() {
        let seek = HlsCommand::Seek {
            segment_index: 10,
            epoch: 1,
        };
        assert!(matches!(seek, HlsCommand::Seek { .. }));

        let force_variant = HlsCommand::ForceVariant {
            variant_index: 2,
            epoch: 2,
        };
        assert!(matches!(force_variant, HlsCommand::ForceVariant { .. }));

        let pause = HlsCommand::Pause;
        assert_eq!(pause, HlsCommand::Pause);

        let resume = HlsCommand::Resume;
        assert_eq!(resume, HlsCommand::Resume);
    }

    #[test]
    fn test_command_equality() {
        let cmd1 = HlsCommand::Seek {
            segment_index: 5,
            epoch: 1,
        };
        let cmd2 = HlsCommand::Seek {
            segment_index: 5,
            epoch: 1,
        };
        let cmd3 = HlsCommand::Seek {
            segment_index: 10,
            epoch: 1,
        };

        assert_eq!(cmd1, cmd2);
        assert_ne!(cmd1, cmd3);

        assert_eq!(HlsCommand::Pause, HlsCommand::Pause);
        assert_ne!(HlsCommand::Pause, HlsCommand::Resume);
    }
}
