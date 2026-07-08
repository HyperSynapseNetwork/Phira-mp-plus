use phira_mp_plus_server::plugin_abi::{plugin_abi_plan, PluginAbiTransport};
use phira_mp_plus_server::runtime_plan::RuntimePlan;

#[test]
fn runtime_plan_tracks_all_required_objectives() {
    let snapshot = RuntimePlan::master_plan().snapshot();
    let keys = snapshot
        .objectives
        .iter()
        .map(|objective| objective.key)
        .collect::<Vec<_>>();

    // Required Runtime v2 objectives (from Phase H spec)
    assert!(keys.contains(&"actor-model"), "must track actor-model");
    assert!(keys.contains(&"plugin-abi-v2"), "must track plugin-abi-v2");
    assert!(keys.contains(&"test-coverage"), "must track test-coverage");
    assert!(
        keys.contains(&"persistence-worker"),
        "must track persistence-worker"
    );
    assert!(keys.contains(&"phira-http"), "must track phira-http");

    // Architectural invariants
    assert!(snapshot.no_web_management_api, "no Web management API");
    assert_eq!(snapshot.final_architecture, "actor_model");

    // Sanity checks
    assert!(
        snapshot.total >= 10,
        "plan should have at least 10 objectives"
    );
    // Active objectives must describe specific remaining blockers.
    // When step-38-closure-gate is done, active must be 0.
    for obj in &snapshot.objectives {
        if obj.status == "active" && obj.key != "step-38-closure-gate" {
            assert!(
                !obj.next_step.starts_with("Keep as")
                    && !obj.next_step.starts_with("Documentation"),
                "active objective '{}' should not be a guardrail: {}",
                obj.key, obj.next_step
            );
        }
    }
}

#[test]
fn plugin_abi_plan_names_wit_only() {
    let plan = plugin_abi_plan();
    assert_eq!(plan.current_transport, PluginAbiTransport::WitTypedV2);
    assert_eq!(plan.target_transport, PluginAbiTransport::WitTypedV2);
    assert_eq!(plan.current_transport.as_str(), "wit_typed_v2");
    assert_eq!(plan.target_transport.as_str(), "wit_typed_v2");
}
