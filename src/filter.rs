//! Message filtering logic for MAVLink messages based on various criteria.
//!
//! This module provides a flexible mechanism to filter incoming and outgoing
//! MAVLink messages on a per-endpoint basis. Filters can be configured to
//! allow or block messages based on their message ID, source component ID,
//! or source system ID.

use mavlink::MavHeader;
use serde::Deserialize;
use std::collections::HashSet;

/// Defines a set of filters to be applied to MAVLink messages for a specific endpoint.
///
/// Filters can be applied to both incoming and outgoing messages.
/// For each criterion (message ID, source component ID, source system ID),
/// you can specify `allow` lists (only messages matching these are passed)
/// or `block` lists (messages matching these are dropped).
///
/// If an `allow` list is specified and is not empty, only messages whose
/// attribute is present in that list will pass.
/// If a `block` list is specified, any message whose attribute is present
/// in that list will be dropped.
///
/// **Note:** Allow lists take precedence over block lists if both are used for the same criterion.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EndpointFilters {
    /// List of MAVLink message IDs allowed for outgoing traffic.
    /// If non-empty, only messages with these IDs will be sent.
    #[serde(default)]
    pub allow_msg_id_out: HashSet<u32>,
    /// List of MAVLink message IDs blocked for outgoing traffic.
    #[serde(default)]
    pub block_msg_id_out: HashSet<u32>,
    /// List of source component IDs allowed for outgoing traffic.
    #[serde(default)]
    pub allow_src_comp_out: HashSet<u8>,
    /// List of source component IDs blocked for outgoing traffic.
    #[serde(default)]
    pub block_src_comp_out: HashSet<u8>,
    /// List of source system IDs allowed for outgoing traffic.
    #[serde(default)]
    pub allow_src_sys_out: HashSet<u8>,
    /// List of source system IDs blocked for outgoing traffic.
    #[serde(default)]
    pub block_src_sys_out: HashSet<u8>,

    /// List of MAVLink message IDs allowed for incoming traffic.
    /// If non-empty, only messages with these IDs will be accepted.
    #[serde(default)]
    pub allow_msg_id_in: HashSet<u32>,
    /// List of MAVLink message IDs blocked for incoming traffic.
    #[serde(default)]
    pub block_msg_id_in: HashSet<u32>,
    /// List of source component IDs allowed for incoming traffic.
    #[serde(default)]
    pub allow_src_comp_in: HashSet<u8>,
    /// List of source component IDs blocked for incoming traffic.
    #[serde(default)]
    pub block_src_comp_in: HashSet<u8>,
    /// List of source system IDs allowed for incoming traffic.
    #[serde(default)]
    pub allow_src_sys_in: HashSet<u8>,
    /// List of source system IDs blocked for incoming traffic.
    #[serde(default)]
    pub block_src_sys_in: HashSet<u8>,
}

impl EndpointFilters {
    /// Checks if an incoming MAVLink message should be allowed based on the defined filters.
    ///
    /// # Arguments
    ///
    /// * `header` - The `MavHeader` of the incoming message.
    /// * `msg_id` - The MAVLink message ID.
    ///
    /// # Returns
    ///
    /// `true` if the message passes all `_in` filters, `false` otherwise.
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

    /// Checks if an outgoing MAVLink message should be allowed based on the defined filters.
    ///
    /// # Arguments
    ///
    /// * `header` - The `MavHeader` of the outgoing message.
    /// * `msg_id` - The MAVLink message ID.
    ///
    /// # Returns
    ///
    /// `true` if the message passes all `_out` filters, `false` otherwise.
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

    /// Internal helper function to apply filtering logic.
    ///
    /// # Arguments
    ///
    /// * `header` - The MAVLink header.
    /// * `msg_id` - The MAVLink message ID.
    /// * `allow_msg` - Allow list for message IDs.
    /// * `block_msg` - Block list for message IDs.
    /// * `allow_comp` - Allow list for component IDs.
    /// * `block_comp` - Block list for component IDs.
    /// * `allow_sys` - Allow list for system IDs.
    /// * `block_sys` - Block list for system IDs.
    ///
    /// # Returns
    ///
    /// `true` if the message passes the filters, `false` otherwise.
    #[allow(clippy::too_many_arguments)]
    fn check(
        header: &MavHeader,
        msg_id: u32,
        allow_msg: &HashSet<u32>,
        block_msg: &HashSet<u32>,
        allow_comp: &HashSet<u8>,
        block_comp: &HashSet<u8>,
        allow_sys: &HashSet<u8>,
        block_sys: &HashSet<u8>,
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
            allow_msg_id_out: HashSet::from([0]), // Only allow Heartbeat (ID 0)
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
            block_msg_id_out: HashSet::from([30]), // Block ATTITUDE (ID 30)
            ..Default::default()
        };

        let header = MavHeader::default();
        assert!(filters.check_outgoing(&header, 0)); // OK
        assert!(!filters.check_outgoing(&header, 30)); // Blocked
    }
}
