use mavlink::common::{MavMessage, CHANGE_OPERATOR_CONTROL_DATA};
use mavrouter_rs::mavlink_utils::extract_target;

#[test]
fn test_change_operator_control_target() {
    let msg = MavMessage::CHANGE_OPERATOR_CONTROL(CHANGE_OPERATOR_CONTROL_DATA {
        target_system: 1,
        control_request: 1,
        version: 0,
        passkey: [0u8; 25],
    });
    let target = extract_target(&msg);
    assert_eq!(target.system_id, 1);
    assert_eq!(target.component_id, 0); // Should be broadcast
}
