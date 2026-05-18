use caap_peg_port as peg;

#[test]
fn default_prefix_blocks_rules_starting_with_invalid_() {
    let grammar = peg::Grammar::new("start <- invalid_alt\ninvalid_alt <- \"x\"");

    let result = peg::parse("x", &grammar.with_start_rule("start"), None, false);
    assert!(result.is_err());
}

#[test]
fn include_invalid_rules_flag_allows_invalid_prefix() {
    let grammar = peg::Grammar::new("start <- invalid_alt\ninvalid_alt <- \"x\"");

    let result = peg::parse(
        "x",
        &grammar.with_start_rule("start"),
        Some(peg::ParserConfig {
            include_invalid_rules: true,
            ..peg::ParserConfig::default()
        }),
        false,
    );
    assert_eq!(result, Ok(peg::ParseValue::Text("x".to_string())));
}

#[test]
fn metadata_replaces_invalid_prefix_default() {
    let grammar = peg::Grammar::new("start <- bad_alt\nbad_alt <- \"x\"").with_start_rule("start");
    let grammar = grammar.with_metadata(
        "__grammar__",
        vec![(
            "invalid_rule_prefixes".to_string(),
            serde_json::json!(["bad_"]),
        )]
        .into_iter()
        .collect(),
    );

    let result = peg::parse("x", &grammar, None, false);
    assert!(result.is_err());
}

#[test]
fn metadata_invalid_prefixes_respected_with_metadata_override() {
    let grammar = peg::Grammar::new("start <- bad_alt\nbad_alt <- \"x\"").with_start_rule("start");
    let grammar = grammar.with_metadata(
        "__grammar__",
        vec![(
            "invalid_rule_prefixes".to_string(),
            serde_json::json!(["bad_"]),
        )]
        .into_iter()
        .collect(),
    );

    let result = peg::parse(
        "x",
        &grammar,
        Some(peg::ParserConfig {
            include_invalid_rules: true,
            ..peg::ParserConfig::default()
        }),
        false,
    );
    assert_eq!(result, Ok(peg::ParseValue::Text("x".to_string())));
}

#[test]
fn empty_metadata_prefix_list_disables_filtering() {
    let grammar =
        peg::Grammar::new("start <- invalid_alt\ninvalid_alt <- \"x\"").with_start_rule("start");
    let grammar = grammar.with_metadata(
        "__grammar__",
        vec![("invalid_rule_prefixes".to_string(), serde_json::json!([]))]
            .into_iter()
            .collect(),
    );

    let result = peg::parse("x", &grammar, None, false);
    assert_eq!(result, Ok(peg::ParseValue::Text("x".to_string())));
}

#[test]
fn invalid_metadata_prefixes_fails_with_parse_error() {
    let grammar = peg::Grammar::new("start <- bad_alt\nbad_alt <- \"x\"").with_start_rule("start");
    let grammar = grammar.with_metadata(
        "__grammar__",
        vec![(
            "invalid_rule_prefixes".to_string(),
            serde_json::json!("bad_"),
        )]
        .into_iter()
        .collect(),
    );

    let result = peg::parse("x", &grammar, None, false);
    assert!(result.is_err());
}

#[test]
fn metadata_from_json_loader_affects_invalid_prefix_filtering() {
    let payload = serde_json::json!({
        "start_rule": "start",
        "rules": [
            {"name": "start", "source": "invalid_alt"},
            {"name": "invalid_alt", "source": "\"x\""},
        ],
        "metadata": {
            "__grammar__": {
                "invalid_rule_prefixes": ["invalid_"]
            }
        }
    });
    let grammar = peg::load_json_grammar(&payload.to_string()).expect("json grammar loads");

    let result = peg::parse("x", &grammar, None, false);
    assert!(result.is_err());
}
