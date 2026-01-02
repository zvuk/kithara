# Kithara — Kanban index

Этот файл больше **не содержит** монолитный kanban. Все задачи разнесены по отдельным документам в `docs/`.

## Как пользоваться

1) Перед началом любой задачи прочитай общие инструкции:
- `docs/kanban-instructions.md`

2) Затем выбери борд по сабкрейту и работай **только** с ним.

- Выполненные пункты отмечаются как `[x]`.
- Невыполненные — `[ ]`.
- Никаких дополнительных статусов (in progress / review и т.д.) — это ведётся вне репозитория.

## Kanban boards (per subcrate)

- `docs/kanban-kithara-core.md`
- `docs/kanban-kithara-cache.md`
- `docs/kanban-kithara-net.md`
- `docs/kanban-kithara-file.md`
- `docs/kanban-kithara-hls.md`
- `docs/kanban-kithara-io.md`
- `docs/kanban-kithara-decode.md`

## Дополнительные документы

- Ограничения и “уроки, которые уже выучили” (EOF, backpressure, offline, ABR и т.д.):
  - `docs/constraints.md`

- Правила для автономных агентов (границы изменений, workspace-first deps, стиль, TDD):
  - `AGENTS.md`

## Примечание

Если в будущем появится новый сабкрейтовый борд, он должен быть добавлен в список выше
в формате `docs/kanban-<crate>.md`.