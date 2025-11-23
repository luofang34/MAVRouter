use mavlink::MavHeader;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EndpointFilters {
    #[serde(default)]
    pub allow_msg_id_out: Vec<u32>,
    #[serde(default)]
    pub block_msg_id_out: Vec<u32>,
    #[serde(default)]
    pub allow_src_comp_out: Vec<u8>,
    #[serde(default)]
    pub block_src_comp_out: Vec<u8>,
    #[serde(default)]
    pub allow_src_sys_out: Vec<u8>,
    #[serde(default)]
    pub block_src_sys_out: Vec<u8>,

    #[serde(default)]
    pub allow_msg_id_in: Vec<u32>,
    #[serde(default)]
    pub block_msg_id_in: Vec<u32>,
    #[serde(default)]
    pub allow_src_comp_in: Vec<u8>,
    #[serde(default)]
    pub block_src_comp_in: Vec<u8>,
    #[serde(default)]
    pub allow_src_sys_in: Vec<u8>,
    #[serde(default)]
    pub block_src_sys_in: Vec<u8>,
}

impl EndpointFilters {
    pub fn check_incoming(&self, header: &MavHeader, msg_id: u32) -> bool {
        Self::check(
            header,
            msg_id,
            &self.allow_msg_id_in,
            &self.block_msg_id_in,
            &self.allow_src_comp_in,
            &self.block_src_comp_in,
            &self.allow_src_sys_in,
            &self.block_src_sys_in,
        )
    }

    pub fn check_outgoing(&self, header: &MavHeader, msg_id: u32) -> bool {
        Self::check(
            header,
            msg_id,
            &self.allow_msg_id_out,
            &self.block_msg_id_out,
            &self.allow_src_comp_out,
            &self.block_src_comp_out,
            &self.allow_src_sys_out,
            &self.block_src_sys_out,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn check(
        header: &MavHeader,
        msg_id: u32,
        allow_msg: &[u32],
        block_msg: &[u32],
        allow_comp: &[u8],
        block_comp: &[u8],
        allow_sys: &[u8],
        block_sys: &[u8],
    ) -> bool {
        // Msg ID
        if !allow_msg.is_empty() && !allow_msg.contains(&msg_id) {
            return false;
        }
        if block_msg.contains(&msg_id) {
            return false;
        }

        // Src Comp
        if !allow_comp.is_empty() && !allow_comp.contains(&header.component_id) {
            return false;
        }
        if block_comp.contains(&header.component_id) {
            return false;
        }

        // Src Sys
        if !allow_sys.is_empty() && !allow_sys.contains(&header.system_id) {
            return false;
        }
        if block_sys.contains(&header.system_id) {
            return false;
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mavlink::MavHeader;

    #[test]
    fn test_filter_logic() {
        let filters = EndpointFilters {
            allow_msg_id_out: vec![0], // Only allow Heartbeat (ID 0)
            ..Default::default()
        };

        let header = MavHeader::default();

        // ID 0 -> Allowed
        assert!(filters.check_outgoing(&header, 0));

        // ID 1 -> Blocked (not in allow list)
        assert!(!filters.check_outgoing(&header, 1));
    }

    #[test]
    fn test_filter_block() {
        let filters = EndpointFilters {
            block_msg_id_out: vec![30], // Block ATTITUDE (ID 30)
            ..Default::default()
        };

        let header = MavHeader::default();
        assert!(filters.check_outgoing(&header, 0)); // OK
        assert!(!filters.check_outgoing(&header, 30)); // Blocked
    }
}
