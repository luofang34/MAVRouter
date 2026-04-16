//! Message filtering logic for MAVLink messages based on various criteria.
//!
//! This module provides a flexible mechanism to filter incoming and outgoing
//! MAVLink messages on a per-endpoint basis. Filters can be configured to
//! allow or block messages based on their message ID, source component ID,
//! or source system ID.

use ahash::AHashSet;
use mavlink::MavHeader;
use serde::Deserialize;

// Use ahash for faster hashing in hot-path filter lookups (issue #18)
type HashSet<T> = AHashSet<T>;

/// Defines a set of filters to be applied to MAVLink messages for a specific endpoint.
///
/// Filters can be applied to both incoming and outgoing messages.
/// For each criterion (message ID, source component ID, source system ID),
/// you can specify `allow` lists (only messages matching these are passed)
/// or `block` lists (messages matching these are dropped).
///
/// ## Filter Evaluation Order
///
/// For each criterion, filters are evaluated in this order:
/// 1. **Allow check**: If allow list is non-empty and message is NOT in the list → reject
/// 2. **Block check**: If message IS in block list → reject
/// 3. Otherwise → pass
///
/// **Note:** Block lists take precedence over allow lists. This enables a "whitelist with
/// exceptions" pattern: define a broad allow list, then use block to exclude specific items.
///
/// ## Example
///
/// ```toml
/// # Allow common telemetry, but exclude high-frequency ATTITUDE messages
/// allow_msg_id_out = [0, 1, 24, 30, 33]
/// block_msg_id_out = [30]  # ATTITUDE blocked even though in allow list
/// ```
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
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
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
    fn test_empty_filters_pass_everything() {
        let filters = EndpointFilters::default();
        let header = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        // All msg IDs should pass both directions
        for msg_id in [0, 1, 30, 100, 255, 65535] {
            assert!(
                filters.check_outgoing(&header, msg_id),
                "outgoing msg_id {msg_id} should pass"
            );
            assert!(
                filters.check_incoming(&header, msg_id),
                "incoming msg_id {msg_id} should pass"
            );
        }
    }

    #[test]
    fn test_allow_msg_id_out_filters() {
        let filters = EndpointFilters {
            allow_msg_id_out: HashSet::from([0, 1, 33]),
            ..Default::default()
        };
        let header = MavHeader::default();

        assert!(filters.check_outgoing(&header, 0));
        assert!(filters.check_outgoing(&header, 1));
        assert!(filters.check_outgoing(&header, 33));
        assert!(!filters.check_outgoing(&header, 30));
        assert!(!filters.check_outgoing(&header, 100));
    }

    #[test]
    fn test_block_msg_id_out_filters() {
        let filters = EndpointFilters {
            block_msg_id_out: HashSet::from([30, 31]),
            ..Default::default()
        };
        let header = MavHeader::default();

        assert!(filters.check_outgoing(&header, 0));
        assert!(filters.check_outgoing(&header, 1));
        assert!(!filters.check_outgoing(&header, 30));
        assert!(!filters.check_outgoing(&header, 31));
    }

    #[test]
    fn test_block_overrides_allow_out() {
        let filters = EndpointFilters {
            allow_msg_id_out: HashSet::from([0, 1, 30]),
            block_msg_id_out: HashSet::from([30]),
            ..Default::default()
        };
        let header = MavHeader::default();

        assert!(filters.check_outgoing(&header, 0));
        assert!(filters.check_outgoing(&header, 1));
        // 30 is in both allow and block — block wins
        assert!(!filters.check_outgoing(&header, 30));
    }

    #[test]
    fn test_allow_msg_id_in_filters() {
        let filters = EndpointFilters {
            allow_msg_id_in: HashSet::from([0, 24]),
            ..Default::default()
        };
        let header = MavHeader::default();

        assert!(filters.check_incoming(&header, 0));
        assert!(filters.check_incoming(&header, 24));
        assert!(!filters.check_incoming(&header, 1));
        assert!(!filters.check_incoming(&header, 30));
    }

    #[test]
    fn test_block_msg_id_in_filters() {
        let filters = EndpointFilters {
            block_msg_id_in: HashSet::from([30]),
            ..Default::default()
        };
        let header = MavHeader::default();

        assert!(filters.check_incoming(&header, 0));
        assert!(!filters.check_incoming(&header, 30));
    }

    #[test]
    fn test_block_overrides_allow_in() {
        let filters = EndpointFilters {
            allow_msg_id_in: HashSet::from([0, 30]),
            block_msg_id_in: HashSet::from([30]),
            ..Default::default()
        };
        let header = MavHeader::default();

        assert!(filters.check_incoming(&header, 0));
        assert!(!filters.check_incoming(&header, 30));
    }

    #[test]
    fn test_allow_sys_id_out() {
        let filters = EndpointFilters {
            allow_src_sys_out: HashSet::from([1, 2]),
            ..Default::default()
        };

        let h_ok = MavHeader {
            system_id: 1,
            component_id: 0,
            sequence: 0,
        };
        let h_blocked = MavHeader {
            system_id: 3,
            component_id: 0,
            sequence: 0,
        };

        assert!(filters.check_outgoing(&h_ok, 0));
        assert!(!filters.check_outgoing(&h_blocked, 0));
    }

    #[test]
    fn test_block_sys_id_out() {
        let filters = EndpointFilters {
            block_src_sys_out: HashSet::from([5]),
            ..Default::default()
        };

        let h_ok = MavHeader {
            system_id: 1,
            component_id: 0,
            sequence: 0,
        };
        let h_blocked = MavHeader {
            system_id: 5,
            component_id: 0,
            sequence: 0,
        };

        assert!(filters.check_outgoing(&h_ok, 0));
        assert!(!filters.check_outgoing(&h_blocked, 0));
    }

    #[test]
    fn test_allow_comp_id_out() {
        let filters = EndpointFilters {
            allow_src_comp_out: HashSet::from([1]),
            ..Default::default()
        };

        let h_ok = MavHeader {
            system_id: 0,
            component_id: 1,
            sequence: 0,
        };
        let h_blocked = MavHeader {
            system_id: 0,
            component_id: 2,
            sequence: 0,
        };

        assert!(filters.check_outgoing(&h_ok, 0));
        assert!(!filters.check_outgoing(&h_blocked, 0));
    }

    #[test]
    fn test_block_comp_id_out() {
        let filters = EndpointFilters {
            block_src_comp_out: HashSet::from([99]),
            ..Default::default()
        };

        let h_ok = MavHeader {
            system_id: 0,
            component_id: 1,
            sequence: 0,
        };
        let h_blocked = MavHeader {
            system_id: 0,
            component_id: 99,
            sequence: 0,
        };

        assert!(filters.check_outgoing(&h_ok, 0));
        assert!(!filters.check_outgoing(&h_blocked, 0));
    }

    #[test]
    fn test_combined_filters() {
        let filters = EndpointFilters {
            allow_msg_id_out: HashSet::from([0, 1]),
            allow_src_sys_out: HashSet::from([1]),
            allow_src_comp_out: HashSet::from([1]),
            ..Default::default()
        };

        // All match — pass
        let h = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        assert!(filters.check_outgoing(&h, 0));

        // msg_id not allowed
        assert!(!filters.check_outgoing(&h, 30));

        // sys_id not allowed
        let h2 = MavHeader {
            system_id: 2,
            component_id: 1,
            sequence: 0,
        };
        assert!(!filters.check_outgoing(&h2, 0));

        // comp_id not allowed
        let h3 = MavHeader {
            system_id: 1,
            component_id: 2,
            sequence: 0,
        };
        assert!(!filters.check_outgoing(&h3, 0));
    }

    #[allow(clippy::unwrap_used)]
    #[test]
    fn test_filter_deserialization() {
        #[derive(Deserialize)]
        struct Wrapper {
            filters: EndpointFilters,
        }

        let toml_str = r#"
[filters]
allow_msg_id_out = [0, 1, 33]
block_msg_id_out = [30]
allow_src_sys_out = [1, 2]
block_src_comp_in = [99]
"#;
        let wrapper: Wrapper = toml::from_str(toml_str).unwrap();
        let f = &wrapper.filters;

        assert!(f.allow_msg_id_out.contains(&0));
        assert!(f.allow_msg_id_out.contains(&1));
        assert!(f.allow_msg_id_out.contains(&33));
        assert_eq!(f.allow_msg_id_out.len(), 3);

        assert!(f.block_msg_id_out.contains(&30));
        assert_eq!(f.block_msg_id_out.len(), 1);

        assert!(f.allow_src_sys_out.contains(&1));
        assert!(f.allow_src_sys_out.contains(&2));

        assert!(f.block_src_comp_in.contains(&99));

        // Default fields should be empty
        assert!(f.allow_msg_id_in.is_empty());
        assert!(f.block_msg_id_in.is_empty());
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

    // ========================================================================
    // Tests migrated from tests/unit_test.rs (empty allows / block overrides
    // allow / allow-only / block-only / component / system / combined).
    // ========================================================================

    #[test]
    fn test_filter_empty_allows_all() {
        let filters = EndpointFilters::default();
        let header = MavHeader {
            system_id: 100,
            component_id: 200,
            sequence: 0,
        };

        assert!(filters.check_incoming(&header, 0));
        assert!(filters.check_incoming(&header, 12345));
        assert!(filters.check_outgoing(&header, 65535));
    }

    #[test]
    fn test_filter_block_overrides_allow() {
        let filters = EndpointFilters {
            allow_msg_id_in: HashSet::from([0, 1]),
            block_msg_id_in: HashSet::from([0]),
            ..Default::default()
        };

        let header = MavHeader::default();

        assert!(!filters.check_incoming(&header, 0)); // allow ∩ block → block wins
        assert!(filters.check_incoming(&header, 1));
        assert!(!filters.check_incoming(&header, 2)); // not in allow
    }

    #[test]
    fn test_filter_allow_list_only() {
        let filters = EndpointFilters {
            allow_msg_id_out: HashSet::from([0, 1, 30]),
            ..Default::default()
        };

        let header = MavHeader::default();

        assert!(filters.check_outgoing(&header, 0));
        assert!(filters.check_outgoing(&header, 1));
        assert!(filters.check_outgoing(&header, 30));
        assert!(!filters.check_outgoing(&header, 2));
    }

    #[test]
    fn test_filter_block_list_only() {
        let filters = EndpointFilters {
            block_msg_id_out: HashSet::from([30, 31, 32]),
            ..Default::default()
        };

        let header = MavHeader::default();

        assert!(filters.check_outgoing(&header, 0));
        assert!(filters.check_outgoing(&header, 1));
        assert!(!filters.check_outgoing(&header, 30));
        assert!(!filters.check_outgoing(&header, 31));
        assert!(!filters.check_outgoing(&header, 32));
    }

    #[test]
    fn test_filter_component() {
        let filters = EndpointFilters {
            allow_src_comp_in: HashSet::from([1, 190]), // autopilot, GCS
            ..Default::default()
        };

        let header_autopilot = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        let header_gcs = MavHeader {
            system_id: 255,
            component_id: 190,
            sequence: 0,
        };
        let header_camera = MavHeader {
            system_id: 1,
            component_id: 100,
            sequence: 0,
        };

        assert!(filters.check_incoming(&header_autopilot, 0));
        assert!(filters.check_incoming(&header_gcs, 0));
        assert!(!filters.check_incoming(&header_camera, 0));
    }

    #[test]
    fn test_filter_system() {
        let filters = EndpointFilters {
            block_src_sys_in: HashSet::from([100, 200]),
            ..Default::default()
        };

        let header_ok = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        let header_blocked1 = MavHeader {
            system_id: 100,
            component_id: 1,
            sequence: 0,
        };
        let header_blocked2 = MavHeader {
            system_id: 200,
            component_id: 1,
            sequence: 0,
        };

        assert!(filters.check_incoming(&header_ok, 0));
        assert!(!filters.check_incoming(&header_blocked1, 0));
        assert!(!filters.check_incoming(&header_blocked2, 0));
    }

    #[test]
    fn test_filter_combined() {
        let filters = EndpointFilters {
            allow_msg_id_in: HashSet::from([0, 30]), // HEARTBEAT and ATTITUDE
            block_src_sys_in: HashSet::from([100]),
            ..Default::default()
        };

        let header_sys1 = MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        let header_sys100 = MavHeader {
            system_id: 100,
            component_id: 1,
            sequence: 0,
        };

        assert!(filters.check_incoming(&header_sys1, 0));
        assert!(filters.check_incoming(&header_sys1, 30));
        assert!(!filters.check_incoming(&header_sys1, 1));
        assert!(!filters.check_incoming(&header_sys100, 0)); // blocked by system
    }
}
