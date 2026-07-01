use phira_mp_plus_server::plugin_abi::{plugin_abi_plan, PluginAbiTransport};
use phira_mp_plus_server::runtime_plan::RuntimePlan;

#[test]
fn runtime_plan_tracks_plugin_abi_and_test_coverage_goals() {
    let snapshot = RuntimePlan::master_plan().snapshot();
    let keys = snapshot
        .objectives
        .iter()
        .map(|objective| objective.key)
        .collect::<Vec<_>>();

    assert!(keys.contains(&"plugin-abi-v2"));
    assert!(keys.contains(&"test-coverage"));
    assert!(snapshot.no_web_management_api);
    assert_eq!(snapshot.final_architecture, "actor_model");
}

#[test]
fn plugin_abi_plan_names_json_bridge_and_wit_target() {
    let plan = plugin_abi_plan();
    assert_eq!(plan.current_transport, PluginAbiTransport::JsonMemoryV1);
    assert_eq!(plan.target_transport, PluginAbiTransport::WitTypedV2);
    assert_eq!(plan.current_transport.as_str(), "json_memory_v1");
    assert_eq!(plan.target_transport.as_str(), "wit_typed_v2");
}
