# Task tracker: board local

Los pendientes viven en el board local (Vercel Board), un channel por repo. Cada comando lleva `board --url http://127.0.0.1:4100/api/board` antes del subcomando y `--as <agente>` al final.

## List

- `board --url http://127.0.0.1:4100/api/board tasks --as <agente>`: tareas abiertas de todos los channels, la más antigua primero.
- `board --url http://127.0.0.1:4100/api/board tasks --state ready --label triage=ready-for-agent --as <agente>`: las que un agente puede tomar ahora (`triage=ready-for-human` para las de moon).

## Add

- `board --url http://127.0.0.1:4100/api/board channels create moon-bindr --as <agente>` (si el channel no existe).
- `board --url http://127.0.0.1:4100/api/board tasks create moon-bindr "<tarea en una línea>" --label triage=<ready-for-agent|ready-for-human> --as <agente>`, con `--label issue=<owner>/<repo>#<n>` o `--label pr=<owner>/<repo>#<n>` si corresponde. Si hay URL, agrégala con `post <n> "<URL completa>" --as <agente>`.
- `triage`: `ready-for-agent` si un agente la termina sin moon; `ready-for-human` si necesita una decisión, una aprobación, una acción hacia afuera o algo indelegable de moon.
- Si la tarea depende de otra tarea o de un hito con fecha: `tasks block <n> "<motivo>" --as <agente>`. El bloqueo dice cuándo se toma; `triage` dice quién. Una tarea existente sin label: `label <n> triage=<valor> --as <agente>`.

## Close

- Terminada: `tasks claim <n> --as <agente> --lease 30s` y luego `tasks complete <n> "<resultado en una línea>" --as <agente>`.
- Descartada: `tasks cancel <n> "<motivo>" --as <agente>`.
