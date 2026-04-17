//! Unit tests for `EndpointFilters`.

#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(clippy::unwrap_used)]

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
