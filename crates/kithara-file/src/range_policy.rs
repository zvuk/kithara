use crate::options::OptionsError;

/// Range and seek policy for file streaming
#[derive(Debug, Clone)]
pub struct RangePolicy {
    current_position: u64,
    total_size: Option<u64>,
}

impl RangePolicy {
    pub fn new(_range_enabled: bool) -> Self {
        Self {
            current_position: 0,
            total_size: None,
        }
    }

    pub fn update_position(&mut self, position: u64) -> Result<(), OptionsError> {
        if let Some(total_size) = self.total_size
            && position > total_size
        {
            return Err(OptionsError::InvalidSeekPosition(position));
        }

        self.current_position = position;
        Ok(())
    }

    pub fn current_position(&self) -> u64 {
        self.current_position
    }

    pub fn is_range_enabled(&self) -> bool {
        true
    }

    pub fn set_total_size(&mut self, size: u64) {
        self.total_size = Some(size);
    }

    pub fn total_size(&self) -> Option<u64> {
        self.total_size
    }

    pub fn should_use_range_for_seek(&self, position: u64) -> bool {
        self.is_valid_position(position)
    }

    pub fn is_valid_position(&self, position: u64) -> bool {
        self.total_size.map_or(true, |total| position <= total)
    }

    pub fn generate_range_header(&self, start_pos: u64) -> Option<String> {
        Some(format!("bytes={}-", start_pos))
    }

    pub fn optimal_buffer_size(&self, default_size: usize) -> usize {
        if let Some(total_size) = self.total_size {
            let remaining = total_size.saturating_sub(self.current_position);
            if remaining < default_size as u64 {
                return remaining as usize;
            }
        }
        default_size
    }
}
