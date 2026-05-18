use caap_peg_port as peg;

#[test]
fn analysis_detects_duplicate_rules() {
    let grammar = peg::Grammar {
        start_rule: "root".to_string(),
        text: "a <- [a]\na <- [b]".to_string(),
        metadata: std::collections::HashMap::new(),
        imports: std::collections::HashMap::new(),
        rules: vec![
            peg::GrammarRule::from_source("a", "[a]", Vec::new()),
            peg::GrammarRule::from_source("a", "[b]", Vec::new()),
        ],
        version: 1,
        state: peg::grammar::GrammarState {
            sealed: false,
            analysis_state: None,
            version: 0,
        },
    };

    let analysis = peg::analyze_grammar(&grammar);
    assert!(analysis.has_duplicate_rule_names);
    assert!(!analysis.errors.is_empty());
}

#[test]
fn mutation_can_add_and_remove_rule() {
    let mut grammar = peg::Grammar::new("a <- [a]");
    assert!(peg::add_rule(&mut grammar, "b", "[b]").is_ok());
    assert!(grammar.get_rule("b").is_some());

    let removed = peg::remove_rule(&mut grammar, "a").expect("remove returns ok");
    assert!(removed);
    assert!(!grammar.text.contains("a <-"));
}

#[test]
fn registry_roundtrips_json_rules() {
    let grammar = peg::load_json_grammar(
        "{\"start_rule\":\"root\",\"rules\":[{\"name\":\"root\",\"source\":\"[a]\"}]}",
    )
    .expect("json grammar loads");

    assert_eq!(grammar.start_rule, "root");
    assert_eq!(grammar.rule_count(), 1);
}

#[test]
fn compile_grammar_rejects_empty_grammar() {
    let grammar = peg::Grammar {
        start_rule: "root".to_string(),
        text: String::new(),
        rules: vec![],
        metadata: std::collections::HashMap::new(),
        imports: std::collections::HashMap::new(),
        version: 1,
        state: peg::grammar::GrammarState {
            sealed: false,
            analysis_state: None,
            version: 0,
        },
    };
    assert!(peg::compile_grammar(&grammar).is_err());
}

#[test]
fn with_rules_resets_analysis_cache() {
    let mut grammar = peg::Grammar::new("a <- [a]");
    let _ = peg::analyze_and_store(&mut grammar);
    assert!(grammar.state.analysis_state.is_some());
    let grammar = grammar.with_rules(vec![peg::GrammarRule::from_source("a", "[b]", Vec::new())]);
    assert!(grammar.state.analysis_state.is_none());
}

#[test]
fn with_start_rule_resets_analysis_cache() {
    let grammar = peg::Grammar::new("a <- [a]").with_start_rule("a");
    let mut grammar = grammar;
    let _ = peg::analyze_and_store(&mut grammar);
    assert!(grammar.state.analysis_state.is_some());

    let grammar = grammar.with_start_rule("b");
    assert!(grammar.state.analysis_state.is_none());
}
