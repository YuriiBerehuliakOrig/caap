# CAAP Module System (`stdlib/module`)

Цей каталог містить реалізацію модульної системи мови CAAP. Вона забезпечує перехід від вихідного коду до повноцінного рантайм-середовища через етапи декларації, валідації та динамічного зв'язування.

## 🏗 Керування станом: `ModuleState`

В основі системи лежить єдиний об'єкт стану `ModuleState`, який координує взаємодію між компілятором та рантаймом.

### Ключові структури даних

| Поле (Map/List) | Тип | Що тримає | Коли заповнюється |
| :--- | :--- | :--- | :--- |
| **`registry`** | Map | Ключ: `module-name`, Значення: `interface-record`. Зберігає "публічний контракт" модуля. | Етап **Validate/Project** |
| **`module_names`** | List | Список рядків — імен усіх зареєстрованих модулів у порядку їх завантаження. | Етап **Validate/Project** |
| **`runtime_models`** | Map | Ключ: `module-name`, Значення: `runtime-model-record`. Містить повний опис імпортів. | Етап **Validate/Project** |
| **`providers`** | Map | Ключ: `module-name`, Значення: `provider-record`. Посилання на реалізацію (host/template). | Етап **Link/Register** |
| **`export_caches`** | Map | Ключ: `module-name`, Значення: Map (`phase` -> `values-map`). Кешовані результати лінкування. | Етап **Link (Runtime)** |

---

## 🔍 Анатомія рекордів (Приклади даних)

### 1. `interface-record` (у `registry`)
Визначає, що модуль виставляє назовні.
```scheme
{
  "module": "stdlib.math",
  "exports": { "abs": true },
  "provenance": {
    "abs": { "kind": "host_service", "library": "math_lib", "export": "native_abs" }
  }
}
```

### 2. `runtime-model-record` (у `runtime_models`)
Описує залежності та публічні символи модуля.
```scheme
{
  "module": "app.main",
  "imports": [ { "kind": "symbol", "module": "stdlib.math", "source": "abs", "local": "my_abs" } ],
  "publics": [ "start_app" ],
  "provenance": { "start_app": { "kind": "top_level", "local": "main_fn" } }
}
```

---

## 🔐 Ключі та Факти (`internal/common_keys.caap`)

Система використовує стандартизовані ключі для зберігання метаданих у вузлах AST:
- `stdlib.module.enabled`: Чи активовано обробку модулів для цього корня.
- `stdlib.module.manifest`: Повний маніфест (імпорти/експорти) після нормалізації.
- `stdlib.module.interface`: Результуючий інтерфейс модуля.
- `stdlib.module.state-version`: Хеш конфігурації для інвалідзації кешів.

---

## 📡 Система подій (`events.caap`)

Всі значущі дії в рантаймі логуються через `module-event`. Кожна подія має:
- **Action**: Назва дії (`link-root`, `register-host-module` тощо).
- **Message**: Опис події для людини.
- **Fields**: Map з технічними деталями (наприклад, `count`, `module_name`, `has_error`).

Валідація в `events.caap` гарантує, що `fields` завжди є коректною мапою.

---

## 🛠 Стандарти розробки

1.  **Сувора Валідація**: Всі аксесори в `internal/common_records.caap` мають вбудовані `assert-record`.
2.  **Монадичність**: Використовуйте `result-bind` для ланцюжкових викликів.
3.  **Розділення відповідальності**: Ключі живуть у `common_keys.caap`, рекорди у `common_records.caap`, а хелпери в `common.caap`.
