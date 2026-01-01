use crate::options::OptionsError;

/// Range and seek policy for file streaming
///
/// This module encapsulates the logic for:
/// - Determining when to use range requests
/// - Validating seek positions
/// - Managing seek state across the driver lifecycle

#[derive(Debug, Clone)]
pub struct RangePolicy {
    /// Whether range requests are enabled
    range_enabled: bool,
    /// Current position in bytes (for seek tracking)
    current_position: u64,
    /// Total file size if known
    total_size: Option<u64>,
}

impl RangePolicy {
    pub fn new(range_enabled: bool) -> Self {
        Self {
            range_enabled,
            current_position: 0,
            total_size: None,
        }
    }

    /// Update the current position after a seek operation
    pub fn update_position(&mut self, position: u64) -> Result<(), OptionsError> {
        if let Some(total_size) = self.total_size {
            if position > total_size {
                return Err(OptionsError::InvalidSeekPosition(position));
            }
        }

        self.current_position = position;
        Ok(())
    }

    /// Get the current position
    pub fn current_position(&self) -> u64 {
        self.current_position
    }

    /// Check if range requests are enabled
    pub fn is_range_enabled(&self) -> bool {
        self.range_enabled
    }

    /// Set the total file size when known
    pub fn set_total_size(&mut self, size: u64) {
        self.total_size = Some(size);
    }

    /// Get the total file size if known
    pub fn total_size(&self) -> Option<u64> {
        self.total_size
    }

    /// Determine if a seek should use range requests
    /// Returns true if range requests are enabled and position is valid
    pub fn should_use_range_for_seek(&self, position: u64) -> bool {
        self.range_enabled && self.is_valid_position(position)
    }

    /// Check if a position is valid for seeking
    pub fn is_valid_position(&self, position: u64) -> bool {
        if let Some(total_size) = self.total_size {
            position <= total_size
        } else {
            // If we don't know the total size, assume it's valid
            true
        }
    }

    /// Generate HTTP Range header value for the given position
    pub fn generate_range_header(&self, start_pos: u64) -> Option<String> {
        if !self.range_enabled {
            return None;
        }

        Some(format!("bytes={}-", start_pos))
    }

    /// Calculate optimal buffer size based on position and file size
    pub fn optimal_buffer_size(&self, default_size: usize) -> usize {
        // If near the end of file and we know the size, use smaller buffer
        if let Some(total_size) = self.total_size {
            let remaining = total_size.saturating_sub(self.current_position);
            if remaining < default_size as u64 {
                return remaining as usize;
            }
        }
        default_size
    }
}
