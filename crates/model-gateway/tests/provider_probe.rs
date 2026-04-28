use model_gateway::types::TranscriptEntry;

#[test]
fn provider_probe_request_uses_fixed_hi_prompt() {
    let req = model_gateway::health::build_probe_request("test-model");

    assert_eq!(req.model, "test-model");
    assert_eq!(req.system, None);
    assert!(req.tools.is_empty());
    assert_eq!(req.max_tokens, 32);
    assert_eq!(req.entries.len(), 1);

    match &req.entries[0] {
        TranscriptEntry::User(user) => {
            assert_eq!(user.text, "hi");
            assert!(user.attachments.is_empty());
        }
        other => panic!("expected user probe entry, got {other:?}"),
    }
}
