# caap peg — Rust port (bootstrap)

Цей каталог містить поетапний порт `peg/` в Rust як crate `caap-peg-port`
у workspace-корені. Мета — паралельно будувати повноцінний Rust-корінь з
однаковими публічними контрактами з Python-референсом там, де він доступний.

## Поточний стан

- Створено модульний Rust crate `caap-peg-port`.
- Додано базовий API-ремінь:
  - `Grammar`, `GrammarState`, `GrammarRule`
  - `ParserConfig`
  - `ParseError`, `ParseSpan`, `ParseValue`, `ParseCache`, `IncrementalEdit`
  - `PEGParser` з:
    - `parse`
    - `parse_prefix`
    - `parse_incremental_many`
    - `snapshot_edits_to_sequential`
    - `clone_grammar`
- Додані портові шари:
  - `analysis` (`analyze_grammar`, `analyze_cached_grammar`, `analyze_and_store`)
  - `mutation` (`add_rule`, `replace_rule`, `remove_rule`, `set_start_rule`)
  - `registry` (`load_json_grammar`, `from_text`)
  - `compile` (мінімальний проєктний конверсійний рівень `NodeSpec`)
- Написано Rust unit/integration тести в `peg/tests`.

## Що вже реалізовано суттєво

- Базова модульність, близька до `peg`-структури.
- Повноцінні структури стану граматики (rules + analysis cache + sealed state).
- Набір аналізаторів/перетворювачів для подальшого масштабування на повний PEG.
- Детальніше: локальний lifecycle граматики, редагування, JSON-завантаження.
- Runtime parser підтримує seed-and-grow виконання для direct/indirect
  left-recursive правил, використовуючи аналіз SCC і `lr_min_step` для
  обмеження growth loop.
- Semantic action/predicate hooks тепер strict-by-default як у Python:
  без переданого `SemanticRuntime` це помилка; для явного pass-through є
  `NullSemanticRuntime`.
- Recovery має fallible `try_recover_parse` і валідатор конфігурації:
  sync tokens або sync regex мають бути задані явно.
- `ImportedRef` і `GrammarScope` можуть резолвитись із `GrammarRegistry`
  через `PEGParser::parse_with_registry` або top-level `parse_with_registry`.
- Додано richer `SemanticContext` і `ContextualSemanticRuntime`: semantic hooks
  отримують span, matched text, args, items, named bindings, start rule, stack і
  позицію.
- `ParserConfig::output_mode` і `parse_output` підтримують `Value`/`Ast` output.
- `SpecCompiler` зберігає behavior entries у runtime `PegExpr`, включно з
  trace capture/action для `parse_ast`.
- Recovery errors отримують absolute line/col після sync recovery.

## Що ще залишилось

1. Довести semantic-runtime context до повного Python payload там, де потрібні
   прямі посилання на grammar/config/state, а не їх Rust-safe projections.
2. Довести incremental pipeline до керування змінами на рівні edit-диапазонів і memo cache.
3. Розширити recovery/діагностику/лінк-правила до повного покриття Python
   `tests/peg`.
4. Під'єднати PyO3-обгортку для прямих викликів з Python-шару.
5. Повідомити/порівняти поведінку з `tests/peg` та поступово замінювати частину виконання.

## Локальна збірка

```bash
cargo build -p caap-peg-port
```

## Локальні тести (Rust)

```bash
cargo test -p caap-peg-port
```

Рекомендується додавати нові тести разом із кожною мірою портингу (API parity тестами).
