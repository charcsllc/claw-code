# Uso de Claw Code

Esta guía cubre el workspace de Rust actual bajo `rust/` y el binario CLI `claw`. Si acabas de llegar, haz que la comprobación de salud del doctor sea tu primera ejecución: inicia `claw` y luego ejecuta `/doctor`.

## Comprobación rápida de salud

Ejecuta esto antes de prompts, sesiones o automatización:

```bash
cd rust
cargo build --workspace
./target/debug/claw
# primer comando dentro del REPL
/doctor
```

`/doctor` es el diagnóstico integrado de configuración y verificación previa. Una vez que tengas una sesión guardada, puedes volver a ejecutarlo con `./target/debug/claw --resume latest /doctor`.

## Requisitos previos

- Toolchain de Rust con `cargo`
- Una de estas opciones:
  - `ANTHROPIC_API_KEY` para acceso directo a la API
  - `ANTHROPIC_AUTH_TOKEN` para autenticación con token bearer
- Opcional: `ANTHROPIC_BASE_URL` cuando apuntes a un proxy o a un servicio local

## Instalar / compilar el workspace

La vía rápida son los instaladores del repo, que compilan **localmente desde este checkout** (sin descargas remotas), verifican el toolchain y hacen un smoke test:

```bash
./install.sh               # Linux / macOS / WSL (añade --release para el optimizado)
```

```powershell
.\install.ps1              # Windows nativo, PowerShell (añade -Release)
```

O manualmente:

```bash
cd rust
cargo build --workspace
```

El binario CLI queda disponible en `rust/target/debug/claw` tras un build de debug (`rust\target\debug\claw.exe` en Windows). Haz que la comprobación del doctor de arriba sea tu primer paso tras el build. Para la instalación orientada a PowerShell, el ZIP de release, el PATH, el cambio de proveedor y ejemplos de notificaciones en Windows/WSL, consulta [`docs/windows-install-release.md`](./docs/windows-install-release.md).

## Inicio rápido

### Comprobación del doctor en la primera ejecución

```bash
cd rust
./target/debug/claw
/doctor
```

O ejecuta doctor directamente con salida JSON para scripting:

```bash
cd rust
./target/debug/claw doctor --output-format json
```

**Nota:** Los verbos de diagnóstico (`doctor`, `status`, `sandbox`, `version`) admiten `--output-format json` para salida legible por máquina. Los argumentos de sufijo inválidos (p. ej., `--json`) ahora se rechazan en tiempo de parseo en lugar de colarse en el despacho de prompts.
`version --output-format json` informa de la procedencia estructurada del build, incluyendo el `git_sha` completo, el `git_sha_short` derivado, `is_dirty`, `branch`, `commit_date`, `commit_timestamp`, `rustc_version`, el `executable_path` en tiempo de ejecución y `binary_provenance`; el JSON mantiene el informe en prosa en `human_readable` en lugar de duplicarlo bajo `message`. `status --output-format json` expone `workspace.memory_files[]` con `path`, `source`, `origin`, `scope_path`, `outside_project`, `chars` y `contributes` para cada archivo de memoria de proyecto cargado.

### Inicializar un repositorio

Configura un repositorio nuevo con `.claw/settings.json`, `.claw.json`, entradas de `.gitignore` y un archivo de guía `CLAUDE.md`:

```bash
cd /path/to/your/repo
./target/debug/claw init
```

El modo texto (legible por humanos) muestra un resumen de la creación de artefactos con la ruta del proyecto y los siguientes pasos. Es idempotente: ejecutarlo varias veces en el mismo repo marca los archivos ya creados como "skipped", informa de `.claw/` como "partial" cuando se materializan sub-archivos que faltaban, y mantiene `.claw/sessions/` en diferido hasta el primer guardado de sesión exitoso.

Modo JSON para scripting:
```bash
./target/debug/claw init --output-format json
```

Devuelve una salida estructurada con los arrays `project_path`, `created[]`, `updated[]`, `partial[]`, `deferred[]` y `skipped[]` (uno por estado de artefacto), y `artifacts[]` que lleva el `name` de cada archivo y su etiqueta `status` estable para máquinas. El campo legado `message` conserva la compatibilidad hacia atrás.

**Por qué importan los campos estructurados:** los claws pueden detectar el estado por artefacto (`created`, `updated`, `partial`, `deferred` o `skipped`) sin hacer matching de subcadenas sobre prosa humana. Usa los arrays de estado para lógica condicional posterior (p. ej., hacer commit solo si los archivos se crearon realmente, no si solo se actualizaron).

### REPL interactivo

```bash
cd rust
./target/debug/claw
```

### Prompt de una sola ejecución

```bash
cd rust
./target/debug/claw prompt "summarize this repository"
```

Envía el texto del prompt por stdin cuando la automatización ya produce el cuerpo del prompt:

```bash
printf 'summarize this repository\n' | ./target/debug/claw prompt --output-format json
```

### Modo de prompt abreviado

```bash
cd rust
./target/debug/claw "explain rust/crates/runtime/src/lib.rs"
```

Usa el separador POSIX `--` de fin de flags cuando el propio prompt abreviado empiece con `-` o `--`:

```bash
./target/debug/claw -- "-summarize this dash-prefixed text"
```

### Salida JSON para scripting

```bash
cd rust
./target/debug/claw --output-format json prompt "status"
```

### Inspeccionar el estado del worker

El comando `claw state` lee `.claw/worker-state.json`, que es escrito por el REPL interactivo o por un prompt de una sola ejecución cuando un worker ejecuta una tarea. Este archivo contiene el ID del worker, la referencia de sesión, el modelo y el modo de permisos.

Requisito previo: debes ejecutar `claw` (REPL interactivo) o `claw prompt <text>` al menos una vez en el repositorio para producir el archivo de estado del worker.

```bash
cd rust
./target/debug/claw state
```

Modo JSON:
```bash
./target/debug/claw state --output-format json
```

Si ejecutas `claw state` antes de que ningún worker se haya ejecutado, verás un error orientativo:
```
error: no worker state file found at .claw/worker-state.json
  Hint: worker state is written by the interactive REPL or a non-interactive prompt.
  Run:   claw               # start the REPL (writes state on first turn)
  Or:    claw prompt <text> # run one non-interactive turn
  Then rerun: claw state [--output-format json]
```

## Comandos slash avanzados (solo REPL interactivo)

Estos comandos están disponibles dentro del REPL interactivo (`claw` sin argumentos). Extienden el asistente con funciones de análisis del workspace, planificación y navegación.

### `/ultraplan` — Planificación profunda con razonamiento en varios pasos

**Propósito:** descomponer una tarea compleja en pasos usando razonamiento extendido.

```bash
# Iniciar el REPL
claw

# Dentro del REPL
/ultraplan refactor the auth module to use async/await
/ultraplan design a caching layer for database queries
/ultraplan analyze this module for performance bottlenecks
```

Salida: un plan estructurado con pasos numerados, el razonamiento de cada paso y los resultados esperados. Úsalo cuando quieras que el asistente piense un problema en detalle antes de programar.

### `/teleport` — Saltar a un archivo o símbolo

**Propósito:** navegar rápidamente a un archivo, función, clase o struct por nombre.

```bash
# Saltar a un símbolo
/teleport UserService
/teleport authenticate_user
/teleport RequestHandler

# Saltar a un archivo
/teleport src/auth.rs
/teleport crates/runtime/lib.rs
/teleport ./ARCHITECTURE.md
```

Salida: el contenido del archivo, con el símbolo solicitado resaltado o el archivo cargado por completo. Útil para explorar el código sin navegar manualmente por directorios. Si hay varias coincidencias, el asistente muestra los mejores candidatos.

### `/bughunter` — Buscar bugs y problemas probables

**Propósito:** analizar el código en busca de errores comunes, antipatrones y bugs potenciales.

```bash
# Analizar todo el workspace
/bughunter

# Analizar un directorio o archivo concreto
/bughunter src/handlers
/bughunter rust/crates/runtime
/bughunter src/auth.rs
```

Salida: una lista de patrones sospechosos con explicaciones (p. ej., "unwrap() sin comprobar", "posible condición de carrera", "falta gestión de errores"). Cada hallazgo incluye el archivo, el número de línea y una corrección sugerida. Úsalo como primera pasada antes de una revisión de código completa.

## Controles de modelo y permisos

```bash
cd rust
./target/debug/claw --model sonnet prompt "review this diff"
./target/debug/claw --permission-mode read-only prompt "summarize Cargo.toml"
./target/debug/claw --permission-mode workspace-write prompt "update README.md"
./target/debug/claw --allowedTools read,glob "inspect the runtime crate"
./target/debug/claw --cwd ../other-workspace status --output-format json
```

Flags globales de sobrescritura del workspace: `--cwd PATH`, `-C PATH` y `--directory PATH` se aceptan antes de cualquier subcomando. Se validan antes del despacho del comando y tienen prioridad sobre el `$PWD` del proceso; las rutas inválidas devuelven errores JSON tipados `invalid_cwd` en modo JSON.

`--allowedTools` acepta nombres canónicos de herramientas en snake_case (por ejemplo `read_file`, `glob_search`, `web_fetch`) además de alias documentados como `read`, `glob`, `Read` y `WebFetch`. `claw status --output-format json` expone `allowed_tools.available` y `allowed_tools.aliases`, y los valores inválidos devuelven JSON tipado `invalid_tool_name` con `tool_name`, `available` y `tool_aliases`. Un valor ausente antes de un subcomando o de otro flag devuelve `missing_argument` con `argument:"--allowedTools"`.

`--output-format` acepta `text` o `json` sin distinguir mayúsculas y normaliza a los modos canónicos en minúsculas. `CLAW_OUTPUT_FORMAT=json` establece el formato de salida por defecto para scripts, mientras que un flag `--output-format` explícito tiene prioridad. Repetir el flag emite una advertencia por stderr y los sobres de estado JSON exponen `format_source`, `format_raw` y `format_overridden` para que los arrays de flags compuestos sean auditables; los valores inválidos devuelven JSON tipado `invalid_output_format` con `value` y `expected:["text","json"]`.

Modos de permisos admitidos (por defecto: `workspace-write`):

- `read-only` permite solo herramientas locales de inspección, como lecturas de archivos, búsquedas glob/grep, skills locales e informes de tipo status. No permite mutar el workspace, herramientas de fetch/búsqueda en red ni la ejecución arbitraria de comandos.
- `workspace-write` es el valor por defecto seguro. Permite lecturas más herramientas de edición directa de archivos dentro del workspace actual, incluidas actualizaciones de write/edit/notebook/config/modo plan, mientras sigue condicionando a una escalada explícita las herramientas de fetch/búsqueda en red, la ejecución arbitraria de shell, el lanzamiento de subagentes, los subprocesos del REPL y otras herramientas de acceso total.
- `danger-full-access` permite todos los requisitos de herramientas registrados, incluida la ejecución arbitraria de comandos, fetch/búsqueda web, lanzamiento de subagentes, REPLs en subproceso y acceso sin restricciones a herramientas. Selecciónalo únicamente con un opt-in explícito mediante `--permission-mode danger-full-access`, `--dangerously-skip-permissions`, `--skip-permissions`, variable de entorno o configuración.

Alias de modelo admitidos actualmente por el CLI:

- `opus` → `claude-opus-4-7`
- `sonnet` → `claude-sonnet-4-6`
- `haiku` → `claude-haiku-4-5-20251213`

## Autenticación

### `/provider` — configuración en un comando (recomendado)

Dentro del REPL, un preset configura el proveedor completo (credencial, base
URL y modelo por defecto), lo **aplica al instante** y lo **persiste** en
`~/.claw/settings.json` (0600) para las próximas sesiones — sin tocar env vars:

```
/provider use zhipu <token-del-coding-plan>     # GLM vía endpoint Anthropic-compat
/provider use kimi <api-key>                    # Moonshot Kimi
/provider use deepseek <api-key>                # DeepSeek (OpenAI-compat)
/provider use qwen <api-key>                    # Alibaba DashScope
/provider use ollama [base-url]                 # local, sin clave
/provider show                                  # qué hay guardado y qué env está activo
/provider test                                  # petición real de 1 token: verifica clave y endpoint
/provider clear                                 # borra el proveedor guardado
```

Las variables de entorno siempre tienen prioridad sobre lo guardado: si
`ANTHROPIC_API_KEY` está en tu shell, se usa esa.

`/provider test [modelo]` hace una petición real mínima al proveedor resuelto:
si la clave es inválida, la base URL está mal o hay un problema de red/proxy,
lo ves aquí con el error exacto — no en tu primer turno de trabajo. Además, si
un turno se queda esperando por un rate-limit (429) u otro error transitorio,
el REPL ahora lo dice en vivo (`⟳ reintento k/9 en Ns`) en vez de parecer
colgado durante el backoff.

### Clave de API

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

### OAuth

```bash
cd rust
export ANTHROPIC_AUTH_TOKEN="anthropic-oauth-or-proxy-bearer-token"
```

### Qué variable de entorno va en cada sitio

`claw` acepta dos variables de entorno de credenciales de Anthropic y **no son intercambiables**: la cabecera HTTP que Anthropic espera difiere según la forma de la credencial. Poner el valor incorrecto en la ranura incorrecta es el 401 más común que vemos.

| Forma de la credencial | Variable de entorno | Cabecera HTTP | Origen típico |
|---|---|---|---|
| Clave de API `sk-ant-*` | `ANTHROPIC_API_KEY` | `x-api-key: sk-ant-...` | [console.anthropic.com](https://console.anthropic.com) |
| Token de acceso OAuth (opaco) | `ANTHROPIC_AUTH_TOKEN` | `Authorization: Bearer ...` | un proxy compatible con Anthropic o un flujo OAuth que emite tokens bearer |
| Clave de OpenRouter (`sk-or-v1-*`) | `OPENAI_API_KEY` + `OPENAI_BASE_URL=https://openrouter.ai/api/v1` | `Authorization: Bearer ...` | [openrouter.ai/keys](https://openrouter.ai/keys) |
| Instancia local de Ollama | `OLLAMA_HOST` | sin cabecera de autenticación (Ollama no requiere ninguna) | servidor Ollama local en `http://127.0.0.1:11434` |

**Por qué importa:** si pegas una clave `sk-ant-*` en `ANTHROPIC_AUTH_TOKEN`, la API de Anthropic devolverá `401 Invalid bearer token` porque las claves `sk-ant-*` se rechazan sobre la cabecera Bearer. La solución es un intercambio de variable de entorno de una línea: mueve la clave a `ANTHROPIC_API_KEY`. Los builds recientes de `claw` detectan exactamente esta forma (401 + `sk-ant-*` en la ranura Bearer) y añaden al mensaje de error una pista que apunta a la solución.

**Si te referías a otro proveedor:** si `claw` informa de que faltan credenciales de Anthropic pero ya tienes exportadas `OPENAI_API_KEY`, `XAI_API_KEY` o `DASHSCOPE_API_KEY`, lo más probable es que hayas olvidado prefijar el nombre del modelo con el prefijo de enrutado del proveedor. Usa `--model openai/gpt-4.1-mini` (compatible con OpenAI / OpenRouter / Ollama), `--model grok` (xAI) o `--model qwen-plus` (DashScope) y el enrutador por prefijos seleccionará el backend correcto independientemente de las credenciales del entorno. El mensaje de error ahora incluye una pista que nombra la variable de entorno detectada.


### Cambio de proveedor en Windows PowerShell

Las mismas reglas de proveedor funcionan en PowerShell. Usa valores de relleno en documentación y tests; pon claves reales solo en tu entorno privado. Elimina las variables de entorno de proveedores no relacionados cuando valides un cambio, para que los fallos sean fáciles de diagnosticar.

`CLAUDE_CODE_PROVIDER` no es necesaria para el enrutado normal de Claw; prefiere prefijos de modelo explícitos como `openai/` y variables de entorno específicas de cada proveedor para que los ejemplos de PowerShell sigan siendo portables.

```powershell
# Anthropic directo
$env:ANTHROPIC_API_KEY = "sk-ant-REPLACE_ME"
Remove-Item Env:\OPENAI_BASE_URL -ErrorAction SilentlyContinue
Remove-Item Env:\OPENAI_API_KEY -ErrorAction SilentlyContinue
.\target\debug\claw.exe --model "sonnet" prompt "reply with ready"

# Gateway compatible con OpenAI / OpenRouter
Remove-Item Env:\ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
$env:OPENAI_BASE_URL = "https://openrouter.ai/api/v1"
$env:OPENAI_API_KEY = "sk-or-v1-REPLACE_ME"
.\target\debug\claw.exe --model "openai/gpt-4.1-mini" prompt "reply with ready"

# Servidor local compatible con OpenAI
$env:OPENAI_BASE_URL = "http://127.0.0.1:11434/v1"
Remove-Item Env:\OPENAI_API_KEY -ErrorAction SilentlyContinue
.\target\debug\claw.exe --model "llama3.2" prompt "reply with ready"
```

Consulta el [quickstart completo de instalación y release en Windows](./docs/windows-install-release.md) para la configuración de artefactos de release, el uso persistente de `setx` y notas sobre WSL.

## Modelos locales

`claw` puede hablar con servidores locales y gateways de proveedores a través de endpoints compatibles con Anthropic o compatibles con OpenAI. Usa `ANTHROPIC_BASE_URL` con `ANTHROPIC_AUTH_TOKEN` para servicios compatibles con Anthropic, o `OPENAI_BASE_URL` con `OPENAI_API_KEY` para servicios compatibles con OpenAI. Para ejemplos copiables de Ollama, llama.cpp, vLLM, `/v1/chat/completions` en crudo e instalación de skills locales, consulta [`docs/local-openai-compatible-providers.md`](./docs/local-openai-compatible-providers.md).

### Endpoint compatible con Anthropic

```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:8080"
export ANTHROPIC_AUTH_TOKEN="local-dev-token"

cd rust
./target/debug/claw --model "claude-sonnet-4-6" prompt "reply with the word ready"
```

### Endpoint compatible con OpenAI

```bash
export OPENAI_BASE_URL="http://127.0.0.1:8000/v1"
export OPENAI_API_KEY="local-dev-token"

cd rust
./target/debug/claw --model "qwen2.5-coder" prompt "reply with the word ready"
```

### Ollama

```bash
export OLLAMA_HOST="http://127.0.0.1:11434"

cd rust
./target/debug/claw --model "llama3.2" prompt "summarize this repository in one sentence"
```

`OLLAMA_HOST` es la variable de entorno preferida. Claw enruta automáticamente todos los modelos al endpoint local de Ollama y no se necesita ninguna clave de API. La solución antigua con `OPENAI_BASE_URL` + `OPENAI_API_KEY` también sigue soportada.

Para tags de Ollama con puntuación (por ejemplo `qwen2.5-coder:7b`), ambos enfoques funcionan:

```bash
export OLLAMA_HOST="http://127.0.0.1:11434"

cd rust
./target/debug/claw --model "qwen2.5-coder:7b" prompt "reply with ready"
```

Si el servidor local expone un ID de modelo que contiene barras, prefíjalo con `local/` para que Claw seleccione el transporte compatible con OpenAI mientras envía el resto textualmente por el cable: `--model "local/Qwen/Qwen3.6-27B-FP8"`.

### OpenRouter

```bash
export OPENAI_BASE_URL="https://openrouter.ai/api/v1"
export OPENAI_API_KEY="sk-or-v1-..."

cd rust
./target/debug/claw --model "openai/gpt-4.1-mini" prompt "summarize this repository in one sentence"
```

### Alibaba DashScope (Qwen)

Para modelos Qwen a través de la API nativa DashScope de Alibaba (límites de tasa más altos que OpenRouter):

```bash
export DASHSCOPE_API_KEY="sk-..."

cd rust
./target/debug/claw --model "qwen/qwen-max" prompt "hello"
# o sin prefijo:
./target/debug/claw --model "qwen-plus" prompt "hello"
```

Los nombres de modelo que empiezan por `qwen/` o `qwen-` se enrutan automáticamente al endpoint de modo compatible de DashScope (`https://dashscope.aliyuncs.com/compatible-mode/v1`). **No** necesitas establecer `OPENAI_BASE_URL` ni eliminar `ANTHROPIC_API_KEY`: el prefijo del modelo gana sobre el detector de credenciales del entorno.

Las variantes de razonamiento (`qwen-qwq-*`, `qwq-*`, `*-thinking`) eliminan automáticamente `temperature`/`top_p`/`frequency_penalty`/`presence_penalty` antes de que la petición salga por el cable (estos parámetros son rechazados por los modelos de razonamiento).

## Proveedores y modelos soportados

`claw` tiene tres backends de proveedor integrados. El proveedor se selecciona automáticamente según el nombre del modelo, con fallback a la credencial que esté presente en el entorno.

### Matriz de proveedores

| Proveedor | Protocolo | Variable(s) de entorno de auth | Variable de entorno de base URL | Base URL por defecto |
|---|---|---|---|---|
| **Anthropic** (directo) | Anthropic Messages API | `ANTHROPIC_API_KEY` o `ANTHROPIC_AUTH_TOKEN` | `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` |
| **xAI** | Compatible con OpenAI | `XAI_API_KEY` | `XAI_BASE_URL` | `https://api.x.ai/v1` |
| **Compatible con OpenAI** | OpenAI Chat Completions | `OPENAI_API_KEY` | `OPENAI_BASE_URL` | `https://api.openai.com/v1` |
| **DashScope** (Alibaba) | Compatible con OpenAI | `DASHSCOPE_API_KEY` | `DASHSCOPE_BASE_URL` | `https://dashscope.aliyuncs.com/compatible-mode/v1` |

El backend compatible con OpenAI también sirve como gateway para **OpenRouter**, **Ollama** y cualquier otro servicio que hable el formato de cable `/v1/chat/completions` de OpenAI: basta con apuntar `OPENAI_BASE_URL` al servicio.

**Enrutado por prefijo del nombre de modelo:** si un nombre de modelo empieza por `openai/`, `local/`, `gpt-`, `qwen/`, `qwen-`, `kimi/` o `kimi-`, el proveedor se selecciona por el prefijo independientemente de qué variables de entorno estén definidas. Esto evita enrutados accidentales hacia Anthropic cuando existen varias credenciales en el entorno. Para la API de OpenAI por defecto y los endpoints locales/privados compatibles con OpenAI, `openai/` es un prefijo de enrutado y se elimina antes de que la petición salga por el cable. Para gateways personalizados no locales con `OPENAI_BASE_URL`, los slugs compatibles con OpenAI que contienen barras (por ejemplo, al estilo OpenRouter `openai/gpt-4.1-mini`) se conservan para que el gateway reciba el ID de modelo que espera. El prefijo `local/` es una vía de escape explícita para IDs de modelo locales con barras: se elimina mientras el resto del ID de modelo se envía textualmente.

### Modelos probados y alias

Estos son los modelos registrados en la tabla de alias integrada con límites de tokens conocidos:

| Alias | Nombre de modelo resuelto | Proveedor | Máx. tokens de salida | Ventana de contexto |
|---|---|---|---|---|
| `opus` | `claude-opus-4-7` | Anthropic | 32 000 | 200 000 |
| `sonnet` | `claude-sonnet-4-6` | Anthropic | 64 000 | 200 000 |
| `haiku` | `claude-haiku-4-5-20251213` | Anthropic | 64 000 | 200 000 |
| `grok` / `grok-3` | `grok-3` | xAI | 64 000 | 131 072 |
| `grok-mini` / `grok-3-mini` | `grok-3-mini` | xAI | 64 000 | 131 072 |
| `grok-2` | `grok-2` | xAI | — | — |
| `kimi` | `kimi-k2.5` | DashScope | 16 384 | 256 000 |
| `qwen-max` | `qwen-max` | DashScope | 8 192 | 131 072 |
| `qwen-plus` | `qwen-plus` | DashScope | 8 192 | 131 072 |
| `gpt-4.1` / `gpt-4.1-mini` / `gpt-4.1-nano` | el mismo | Compatible con OpenAI | 32 768 | 1 047 576 |
| `gpt-5.4` / `gpt-5.4-mini` / `gpt-5.4-nano` | el mismo | Compatible con OpenAI | 128 000 | 1 000 000 / 400 000 |

Cualquier nombre de modelo que no coincida con un alias se pasa textualmente una vez resuelto el enrutado de proveedor. Así es como usas slugs de modelo de OpenRouter (`openai/gpt-4.1-mini` con un `OPENAI_BASE_URL` personalizado), tags de Ollama (`llama3.2` o `qwen2.5-coder:7b`), IDs locales con barras (`local/Qwen/Qwen3.6-27B-FP8`) o IDs de modelo completos de Anthropic (`claude-sonnet-4-20250514`).

### Alias definidos por el usuario

Puedes añadir alias personalizados en cualquier archivo de configuración (`~/.claw/settings.json`, `.claw/settings.json` o `.claw/settings.local.json`):

```json
{
  "aliases": {
    "fast": "claude-haiku-4-5-20251213",
    "smart": "claude-opus-4-7",
    "cheap": "grok-3-mini"
  }
}
```

La configuración local del proyecto sobrescribe la configuración a nivel de usuario. Los alias se resuelven a través de la tabla integrada, así que `"fast": "haiku"` también funciona.

La precedencia de selección de modelo es: flag del CLI, entorno, configuración y, por último, el valor por defecto. La ranura de modelo del entorno acepta `CLAW_MODEL`, `ANTHROPIC_MODEL` y `ANTHROPIC_DEFAULT_MODEL` en ese orden; los alias procedentes de esas variables se resuelven y validan antes del arranque del proveedor. `claw --output-format json status` expone `model_raw`, `model_alias_resolved_to` y `model_env_var` para que la automatización pueda ver el valor ganador.

### Cómo funciona la detección de proveedor

1. Si el nombre de modelo resuelto empieza por `claude` → Anthropic.
2. Si empieza por `grok` → xAI.
3. Si empieza por `openai/`, `local/` o `gpt-` → compatible con OpenAI.
4. Si empieza por `qwen/`, `qwen-`, `kimi/` o `kimi-` → formato de cable OpenAI compatible con DashScope.
5. Si `OPENAI_BASE_URL` está definida, los nombres de modelo desconocidos con aspecto local, como `llama3.2` o `qwen2.5-coder:7b`, se enrutan al cliente compatible con OpenAI para servidores locales/gateway.
6. En caso contrario, `claw` comprueba qué credencial está definida: primero Anthropic, luego OpenAI y después xAI. Si solo `OPENAI_BASE_URL` está definida, sigue enrutando a compatible con OpenAI para servidores locales sin autenticación.
7. Si nada coincide, usa Anthropic por defecto.


### Diagnósticos de proveedor y parámetros personalizados compatibles con OpenAI

La capa de API expone una instantánea de diagnósticos de proveedor mediante `api::provider_diagnostics_for_model(model)`. Informa del proveedor resuelto, las variables de entorno de auth/base-url, la base URL por defecto, si el proveedor usa el formato de cable compatible con OpenAI, si se eliminan los parámetros de ajuste de razonamiento, si se conserva el historial de razonamiento de DeepSeek V4, el soporte de proxy, el soporte de extra-body y si los IDs de modelo con barras se conservan para gateways personalizados compatibles con OpenAI.

Para funciones de gateway que aún no son campos de primera clase en la petición, `MessageRequest::extra_body` pasa parámetros JSON específicos del proveedor, como `web_search_options` o `parallel_tool_calls`. Los campos centrales del protocolo (`model`, `messages`, `stream`, `tools`, `tool_choice`, `max_tokens` y `max_completion_tokens`) están protegidos y no se pueden sobrescribir a través de `extra_body`.

## Contexto de archivos y navegación

Usa `@path/to/file` en los prompts para enviar archivos del repositorio como contexto, por ejemplo `Read @src/app.ts and explain the bug`, `Compare @old.md and @new.md` o `Use @logs/error.txt as context and suggest a fix`. El historial de prompts, `Ctrl-r` y el desplazamiento en salidas largas provienen de tu shell, terminal o tmux, no de Claw. Consulta [`docs/navigation-file-context.md`](./docs/navigation-file-context.md) para orientación sobre scrollback, adjuntos y redacción de secretos.

## FAQ

### ¿Claw Code es solo para Claude?

No. Claw Code es un workflow/runtime con la forma de Claude Code, no un producto exclusivo de Claude. Puede apuntar a modelos de Anthropic y a modelos compatibles con OpenAI, enrutados por proveedor o locales, según la configuración. Los proveedores no-Claude pueden requerir una compatibilidad más estricta en la forma de las respuestas y en las llamadas a herramientas, por lo que algunos workflows pueden ser más ásperos que las rutas de primera parte de Anthropic/OpenAI; las fugas de identidad específicas de un proveedor son bugs, no intención del producto. Consulta [`docs/local-openai-compatible-providers.md`](./docs/local-openai-compatible-providers.md) para ejemplos de proveedores locales.

### ¿Y qué hay de Codex?

El nombre "codex" aparece en el ecosistema de Claw Code pero **no** se refiere a OpenAI Codex (el modelo de generación de código). Esto es lo que significa en este proyecto:

- **`oh-my-codex` (OmX)** es la capa de workflow y plugins que se apoya sobre `claw`. Proporciona modos de planificación, ejecución multiagente en paralelo, enrutado de notificaciones y otras funciones de automatización. Consulta [PHILOSOPHY.md](./PHILOSOPHY.md) y el [repo de oh-my-codex](https://github.com/Yeachan-Heo/oh-my-codex).
- Los **directorios `.codex/`** (p. ej. `.codex/skills`, `.codex/agents`, `.codex/commands`) son rutas de búsqueda legadas que `claw` sigue escaneando junto a los directorios primarios `.claw/`.
- **`CODEX_HOME`** es una variable de entorno opcional que apunta a una raíz personalizada para las búsquedas de skills y comandos a nivel de usuario.

`claw` **no** soporta sesiones de OpenAI Codex, el Codex CLI ni la importación/exportación de sesiones de Codex. Si necesitas usar modelos de OpenAI (como GPT-4.1), configura el proveedor compatible con OpenAI como se muestra arriba en las secciones [Endpoint compatible con OpenAI](#openai-compatible-endpoint) y [OpenRouter](#openrouter).

## Soporte de proxy HTTP

`claw` respeta las variables de entorno estándar `HTTP_PROXY`, `HTTPS_PROXY` y `NO_PROXY` (se aceptan tanto en mayúsculas como en minúsculas) al emitir peticiones salientes hacia endpoints compatibles con Anthropic, OpenAI y xAI. Defínelas antes de lanzar el CLI y el cliente `reqwest` subyacente se configurará automáticamente.

### Variables de entorno

```bash
export HTTPS_PROXY="http://proxy.corp.example:3128"
export HTTP_PROXY="http://proxy.corp.example:3128"
export NO_PROXY="localhost,127.0.0.1,.corp.example"
export CLAW_OUTPUT_FORMAT="json"   # formato de salida no interactivo por defecto; los flags lo sobrescriben
export CLAW_LOG="debug"             # selector de nivel de log específico de claw expuesto por help/doctor
export RUST_LOG="claw=debug"        # convención de logging de Rust expuesta por help/doctor

cd rust
./target/debug/claw prompt "hello via the corporate proxy"
```

### Opción de configuración programática `proxy_url`

Como alternativa a las variables de entorno por esquema, el tipo `ProxyConfig` expone un campo `proxy_url` que actúa como proxy único para todo el tráfico HTTP y HTTPS. Cuando `proxy_url` está definido, tiene prioridad sobre los campos separados `http_proxy` y `https_proxy`.

```rust
use api::{build_http_client_with, ProxyConfig};

// Desde una única URL unificada (archivo de configuración, flag del CLI, etc.)
let config = ProxyConfig::from_proxy_url("http://proxy.corp.example:3128");
let client = build_http_client_with(&config).expect("proxy client");

// O define el campo directamente junto a NO_PROXY
let config = ProxyConfig {
    proxy_url: Some("http://proxy.corp.example:3128".to_string()),
    no_proxy: Some("localhost,127.0.0.1".to_string()),
    ..ProxyConfig::default()
};
let client = build_http_client_with(&config).expect("proxy client");
```

### Notas

- Cuando tanto `HTTPS_PROXY` como `HTTP_PROXY` están definidas, el proxy seguro se aplica a las URLs `https://` y el proxy plano a las URLs `http://`.
- `proxy_url` es una alternativa unificada: cuando está definido, se aplica tanto a destinos `http://` como `https://`, sobrescribiendo los campos por esquema.
- `NO_PROXY` acepta una lista separada por comas de sufijos de host (por ejemplo `.corp.example`) y literales IP.
- Los valores vacíos se tratan como no definidos, así que dejar `HTTPS_PROXY=""` en tu shell no activará ningún proxy.
- Si una URL de proxy no se puede parsear, `claw` recurre a un cliente directo (sin proxy) para que los workflows existentes sigan funcionando; revisa la URL si esperabas que la petición se tunelizara.

## Skills

Usa `/skills list` en el REPL interactivo o `claw skills --output-format json` desde el CLI directo para inspeccionar las skills instaladas. Para instalaciones offline/locales, instala el directorio que contiene `SKILL.md` y luego verifica el nombre descubierto antes de invocarla. `skills install`, `skills uninstall` y `agents create` son comandos de ciclo de vida sobre el sistema de archivos local; no requieren credenciales de proveedor.

```text
/skills install /absolute/path/to/my-skill
/skills list
/skills uninstall my-skill
/skills my-skill
```

Si la instalación tiene éxito pero la invocación falla con un error HTTP del proveedor, trata la configuración del proveedor por separado: ejecuta `claw doctor` y una prueba rápida con un prompt de una sola ejecución antes de reinstalar la skill. Consulta [`docs/local-openai-compatible-providers.md`](./docs/local-openai-compatible-providers.md#local-skills-install-from-disk) para la lista de comprobación completa.

## Comandos operativos comunes

```bash
cd rust
./target/debug/claw status
./target/debug/claw sandbox
./target/debug/claw agents
./target/debug/claw agents create my-agent
./target/debug/claw mcp
./target/debug/claw skills
./target/debug/claw system-prompt --cwd .. --date 2026-04-04
```

## Instalar una skill externa

`claw skills install <path>` acepta un directorio de skill local que contenga
`SKILL.md` o un archivo markdown independiente. Esto es útil cuando un
repositorio complementario incluye un prompt de skill que debería estar disponible a través de `/skills`.

Por ejemplo, instala TweetClaw como skill de automatización para X/Twitter:

```bash
# From a parent directory that contains claw-code
git clone https://github.com/Xquik-dev/tweetclaw
cd claw-code/rust
./target/debug/claw skills install ../../tweetclaw/skills/tweetclaw
./target/debug/claw skills show tweetclaw
./target/debug/claw skills uninstall tweetclaw
```

TweetClaw ofrece a los usuarios de `claw` una guía de skill local para workflows de OpenClaw/Xquik
como búsqueda de tweets, búsqueda de respuestas, exportación de seguidores, monitores, webhooks y
publicación condicionada a aprobación. Configura cualquier credencial de Xquik fuera del prompt y
evita pegar claves de API en el chat.

## Crear un agente local

`claw agents create <name>` genera el andamiaje de un archivo local `.claw/agents/<name>.toml` para el workspace actual. El andamiaje es intencionadamente pequeño para que puedas editar la descripción, el modelo y el esfuerzo de razonamiento antes de listar o invocar agentes:

```bash
./target/debug/claw agents create release-checker
./target/debug/claw agents list
```

## Gestión de sesiones

Los turnos del REPL se persisten bajo `.claw/sessions/` en el workspace actual.

```bash
cd rust
./target/debug/claw --resume latest
./target/debug/claw --resume latest /status /diff
```

Entre los comandos interactivos útiles están `/help`, `/status`, `/cost`, `/usage`, `/context`, `/config`, `/session`, `/model`, `/permissions` y `/export`. `/status` muestra también el proveedor activo y su base URL (útil con `/provider`); `/usage` desglosa los tokens de la sesión incluyendo el último turno; `/context` indica el % usado de la ventana de contexto del modelo y cuán cerca estás del umbral de auto-compactación para poder ejecutar `/compact` a tiempo.

Más comandos de sesión y utilidad:

- `/effort low|medium|high|off` — cambia el esfuerzo de razonamiento en vivo (persiste durante toda la sesión).
- `/copy` — copia la última respuesta al portapapeles del sistema; `/copy all` copia toda la conversación en markdown (usa wl-copy/xclip/xsel/pbcopy/clip.exe, el primero disponible).
- `/branch [nombre]` — bifurca la sesión actual a un archivo nuevo (la actual sigue intacta); retómala con `claw --resume <id>`.
- `/rewind [n]` — descarta los últimos n intercambios (por defecto 1): el modelo deja de verlos.
- `/files` — rama actual y archivos cambiados del árbol de trabajo (git status).
- `/keybindings` — atajos reales del editor de línea (historial, búsqueda inversa, multilínea…).
- `/upgrade` — versión, commit del binario y comandos exactos de actualización.
- `/plan <petición>` — turno de planificación con las **herramientas desactivadas**: el modelo solo puede pensar, no ejecutar; ideal antes de un cambio grande.
- `/review [staged]` — revisión con IA del diff del árbol de trabajo (o del staged), con veredicto ship/fix-first.
- `/fast` — cambia al modelo rápido configurado (`subagentModel` de `/setup`) y con otro `/fast` vuelves al anterior.
- `/security-review` — checks deterministas de 0 tokens: escaneo de credenciales hardcodeadas y `.env` commiteados.
- `/release-notes` — los últimos 20 commits del checkout que sirve tu binario.
- `/privacy-settings` — dónde vive cada dato local (settings, sesiones, telemetría) y cómo purgarlo.
- `/history search <término>` — busca en el historial de prompts en vez de listar los últimos.
- `/summary` — la sesión de un vistazo: modelo, proveedor, effort, mensajes, turnos, tokens, coste y último prompt.
- `/hooks` — hooks configurados por evento, incluyendo los que no parsearon (y por qué).
- `/color on|off|auto` (también `/theme`) — fuerza o desactiva los colores ANSI en vivo; `off` silencia también el spinner.
- `/usage last` — tokens solo del último turno.

Además: el banner de arranque muestra la versión y el proveedor activo (y avisa si no hay credenciales); `/doctor` muestra el proveedor, el timeout de bash efectivo y avisa si `settings.json` o el directorio de sesiones quedaron legibles por otros usuarios; los archivos de sesión se guardan con permisos 0600; el historial de prompts se limita a 1000 entradas; `CLAW_BASH_TIMEOUT_MS` ajusta el timeout por defecto de la herramienta bash; el spinner indica el modelo (y effort) que está pensando y el «Done» final incluye la duración del turno.

Más calidad de vida: cuando un turno falla, el error viene con una **pista accionable** (401→`/provider test`, 429→espera/cambia proveedor, contexto lleno→`/compact`, red→revisa Base URL); `/cost` desglosa coste de input/output; `/usage` muestra el **hit-rate de la caché de prompts** y `/usage last` el coste del último turno; `/context` dibuja una barra de utilización; `/color` **persiste entre sesiones**; `/model` avisa cuando el nuevo modelo cambia de proveedor; `/rewind` te dice qué prompt eliminó; `/provider test` tiene timeout de 30s; el preset `openrouter` (alias `or`) se une a la lista; el wizard de `/setup` muestra el proveedor activo, aplica los cambios sin reiniciar y ofrece probar la conexión al terminar; y `edit_file` sugiere dónde quedó el ancla cuando un bloque no coincide.

**Subsistema de diseño gráfico** (builds `/web`/`/app`): el pipeline detecta el **arquetipo de producto** (ecommerce, dashboard, landing, SaaS, editorial, fintech, social, reservas o general) y dirige al Diseñador UX/UI con una dirección de arte específica por arquetipo; antes de que el agente toque nada, se escribe una **fundación de tokens validada** (`src/styles/design-tokens.css`: superficies, tinta, marca, 4 estados, 8 slots de datos con orden seguro para daltonismo, escala tipográfica 1.25, spacing 4px, radios, sombras, z-index, movimiento y focus ring — tema claro y oscuro) cuyos pares texto/superficie **cumplen WCAG AA por construcción**; el agente de design system recibe un contrato de componentes con todos sus estados (hover/focus-visible/disabled/loading/error/empty, targets táctiles ≥44px, aria); el QA visual audita contra una **rúbrica de 10 puntos** sobre el DOM renderizado real; y un **gate de diseño determinista de 0 tokens** recalcula el contraste WCAG de los tokens y audita el HTML renderizado (lang, viewport, alt, h1 único, landmark main, enlaces muertos, lorem ipsum, title) despachando al Técnico si algo falla. Junto a los tokens se escribe un **base.css de suelo de accesibilidad** (focus-visible, selection, reduced-motion, `.visually-hidden`, targets táctiles) que existe aunque el agente se quede corto; el gate añade **disciplina de tokens** (detecta colores hex hardcodeados en CSS de componentes), etiqueta cada hallazgo por categoría (`[CONTRASTE]`/`[A11Y]`/`[TOKENS]`) y **re-verifica tras el QA visual** (solo informe, para no entrar en bucle). Si el proyecto ya trae tokens propios, se respetan — y en `/improve` el gate nunca exige adoptar los nuestros.

En los builds multiagente: `--timeout-secs N` ajusta el timeout por agente desde el REPL, `--parallel` se acota a 1–16, el build **falla rápido** si no hay credenciales de proveedor en el entorno (antes moría dentro del Director con un error confuso), y el resultado queda persistido en `docs/SUMMARY.md`. El build gate detecta **pnpm/yarn por su lockfile** (antes siempre npm); el presupuesto de bundle se ajusta con `CLAW_PERF_BUDGET_KB` y el timeout del smoke test con `CLAW_SMOKE_TIMEOUT_SECS`; los modelos por rol admiten overrides `CLAW_MA_{SIMPLE,MEDIUM,COMPLEX,DIRECTOR,SUPERVISOR}_MODEL`; las escaladas de reintento quedan auditadas en `docs/SUPERVISION.md`; y el digest del repo reconoce más ecosistemas (pnpm, Docker, Makefile, Deno, Composer, Gemfile).

Atajos y detalles nuevos del REPL: los alias `/model glm|kimi|deepseek|qwen` resuelven al modelo insignia de cada proveedor; escribir `exit`, `quit` o `salir` (sin barra) también cierra la sesión y `?` abre la ayuda; `/provider use` **rechaza claves placeholder** (`<token>`, `...`) antes de persistirlas; y hay más pistas de error accionables (cuota/saldo agotado, modelo inexistente, proveedor sobrecargado, fallo TLS de proxy). El renderizado markdown pinta ahora listas de tareas (`☐`/`☑`), reglas horizontales y texto tachado.

## Orden de resolución de los archivos de configuración

La configuración en tiempo de ejecución se carga en este orden, con las entradas posteriores sobrescribiendo a las anteriores:

1. `~/.claw.json`
2. `~/.config/claw/settings.json`
3. `<repo>/.claw.json`
4. `<repo>/.claw/settings.json`
5. `<repo>/.claw/settings.local.json`

La lista es también la cadena de precedencia: la configuración local del proyecto sobrescribe la configuración del proyecto, la configuración del proyecto sobrescribe el `.claw.json` legado del proyecto, y los archivos del proyecto sobrescriben los archivos del usuario. `claw --output-format json config` incluye para cada archivo descubierto `precedence_rank`, `wins_for_keys` y `shadowed_keys`, de modo que la automatización pueda ver qué archivo controla cada clave efectiva sin reimplementar el orden de fusión.

## Instalar servidores MCP

`claw mcp add` escribe por ti la entrada `mcpServers` — sin editar JSON a mano:

```bash
# servidor stdio: todo lo que sigue al nombre es la línea de comandos
claw mcp add memoria npx -y codebase-memory-mcp

# servidor remoto: una URL se detecta automáticamente como transporte HTTP (--sse para SSE)
claw mcp add remoto https://ejemplo.com/mcp
claw mcp add eventos https://ejemplo.com/sse --sse

# opciones: --env K=V (stdio), --header K=V (remoto),
#          --scope local|project (por defecto local), --force (sobrescribir)
claw mcp add api npx api-mcp --env API_KEY=xyz --scope project

claw mcp remove memoria     # lo elimina de todos los archivos de configuración
```

`add` apunta por defecto a `.claw/settings.local.json` (local a la máquina) o a
`.claw/settings.json` con `--scope project` (compartido, apto para commit). Tras
escribir, revalida la configuración completa y revierte el archivo intacto
si la nueva entrada no parsea. Ambos verbos también funcionan como `/mcp add ...`
dentro de una sesión y respetan `--output-format json`.

## Validación de servidores MCP

`claw mcp --output-format json` carga las entradas `mcpServers` válidas incluso cuando entradas hermanas están malformadas. El sobre JSON de la lista distingue el total de entradas configuradas de los subconjuntos válidos e inválidos:

```json
{
  "configured_servers": 1,
  "total_configured": 2,
  "valid_count": 1,
  "invalid_count": 1,
  "servers": [{ "name": "valid-server", "valid": true }],
  "invalid_servers": [
    {
      "name": "missing-command",
      "error_field": "command",
      "reason": ".claw.json: mcpServers.missing-command: missing string field command",
      "valid": false
    }
  ]
}
```

`status --output-format json` refleja esto bajo `mcp_validation`, y `doctor --output-format json` incluye una comprobación `mcp validation` para que la automatización pueda reparar cada entrada de servidor rechazada sin perder los servidores MCP utilizables.

## Configuración de hooks

`hooks.PreToolUse`, `hooks.PostToolUse` y `hooks.PostToolUseFailure` aceptan tanto cadenas de comando legadas como entradas de estilo objeto con un `matcher` y hooks de comando anidados:

```json
{
  "hooks": {
    "PreToolUse": [
      "echo legacy hook",
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "scripts/audit-bash.sh" }
        ]
      }
    ]
  }
}
```

Los matchers de estilo objeto son opcionales. Cuando están presentes, hacen matching de los nombres de herramientas sin distinguir mayúsculas y admiten comodines `*` además de alternativas separadas por comas o barras verticales. El `type` del hook anidado puede omitirse o establecerse a `"command"`; cada comando anidado se ejecuta en el orden de la configuración.
Las entradas de hook legadas de cadena simple siguen cargándose por compatibilidad hacia atrás, pero emiten advertencias de obsolescencia que sugieren migrar a entradas de estilo objeto. Los nombres de eventos de hook desconocidos (p. ej. `Stop`, `Notification`) se registran como inválidos sin rechazar los hooks válidos. `status --output-format json` refleja la validación parcial de hooks bajo `hook_validation` con `valid_count`, `invalid_count` e `invalid_hooks:[{event, index, hook_index, kind, error_field, reason, valid:false}]`. `doctor --output-format json` incluye una comprobación `hook validation` para que la automatización pueda reparar cada entrada de hook rechazada sin perder los hooks utilizables.

## Reglas de instrucciones del proyecto

Además de los archivos de instrucciones raíz como `CLAUDE.md`, `CLAW.md`, `AGENTS.md`, `.claw/CLAUDE.md`, `.claude/CLAUDE.md` y `.claw/instructions.md`, `claw` carga archivos de reglas Markdown/texto ordenados desde:

- `<repo>/.claw/rules/` (`.md`, `.txt`, `.mdc`) para reglas de proyecto compartidas.
- `<repo>/.claw/rules.local/` para reglas locales personales; esta ruta está en gitignore.

La prioridad de los archivos de instrucciones raíz es `CLAUDE.md`, luego `CLAW.md` y después `AGENTS.md` para cada directorio descubierto. El descubrimiento está acotado a la raíz git actual cuando existe una, y en caso contrario solo al directorio actual, de modo que archivos padre obsoletos fuera del proyecto no se cuelen silenciosamente en el prompt. Todos los archivos cargados contribuyen al prompt del sistema y a `status --output-format json` como `workspace.memory_files:[{path, source, origin, scope_path, outside_project, chars, contributes}]`; `claw doctor --output-format json` incluye una comprobación `memory` para que la automatización pueda detectar candidatos de archivos de memoria cargados y no cargados de forma inesperada sin parsear el texto del prompt.

Por defecto, `claw` también importa reglas detectadas de herramientas de programación con IA comunes, como Cursor (`.cursorrules`, `.cursor/rules/`), GitHub Copilot (`.github/copilot-instructions.md`), Windsurf, Plandex y Crush. Controla esto con `rulesImport` en cualquier archivo de configuración:

```json
{
  "rulesImport": "none"
}
```

Usa `"auto"` (el valor por defecto) para importar todos los frameworks soportados, `"none"` para cargar solo los archivos de instrucciones/reglas de Claw, o un array como `["cursor", "copilot"]` para importar frameworks seleccionados.

## Harness de parity con mock

El workspace incluye un servicio mock determinista compatible con Anthropic y un harness de parity.

```bash
cd rust
./scripts/run_mock_parity_harness.sh
```

Arranque manual del servicio mock:

```bash
cd rust
cargo run -p mock-anthropic-service -- --bind 127.0.0.1:0
```

## Builds multiagente autónomos (WEB / APP)

`claw-multiagent` convierte un solo prompt en un proyecto completo mediante una
jerarquía de agentes especializados. El pipeline, en orden:

1. **Director General** — interpreta el prompt, crea el plan (visión, alcance,
   stack) y resuelve él mismo cualquier pregunta abierta, documentando cada
   decisión en `docs/decisions.md`.
2. **Arquitectos en paralelo** (software, frontend, backend, DevOps, UX/UI) —
   diseñan la solución antes de que exista el backlog.
3. **Subdirector Técnico** — convierte plan + diseños en TaskSpecs
   completamente especificados (con validación estructural y una ronda de
   reparación automática si el backlog no valida).
4. **Scaffold determinista** — `create-vite` / `cargo init` / `npm init` según
   el stack: el proyecto compila desde el minuto cero y los developers solo
   escriben código de producto (desactívalo con `--no-scaffold`).
5. **Contratos como código** — un arquitecto escribe los archivos reales de
   tipos/interfaces/rutas de API (catalogados en `docs/contracts.md`); la
   consistencia entre módulos la impone el typechecker, no la prosa.
6. **Scheduler por grafo, sin barreras** — cada tarea arranca en cuanto sus
   dependencias terminan y ningún agente en vuelo tiene sus archivos
   (aislamiento duro de escritura por módulo); los fallos se reintentan una
   vez escalando al siguiente nivel de modelo, y cada entrega pasa una
   verificación rápida (`tsc --noEmit` / `cargo check`) al aterrizar.
7. **Supervisor en paralelo** — revisa cada entrega en `docs/SUPERVISION.md`
   mientras el resto sigue construyendo, y despacha un Fixer en un modelo
   superior cuando encuentra issues. Cada tarea aprobada genera un commit de
   git propio.
8. **Build gate final + QA + smoke test + Documentación** — build completo
   con reparación automática, tests reales ejecutados (y reparados si
   fallan), arranque del dev server con petición HTTP de verificación
   (`docs/smoke-test.md`) y README/ADRs/changelog.

Dentro de una sesión interactiva de claw, simplemente escribe:

```
/web un ecommerce para vender productos electrónicos --dry-run
/app app de notas offline para Android [--parallel N] [--output <dir>]
/improve añade un carrito con Stripe   # sobre el proyecto del directorio actual
```

**Modo `/improve` (proyecto existente).** Mismo pipeline, pero en vez de crear
un proyecto desde cero, opera sobre uno que ya existe: analiza el repo (árbol de
archivos + manifiestos), el Director planifica **solo el cambio pedido** (sin
re-planificar el producto), y las olas de developers lo implementan respetando el
stack y las convenciones detectadas — prefiriendo modificar archivos existentes
antes que crear nuevos. Se saltan las fases de andamiaje greenfield (scaffold,
contratos, design system, seed data, pack de deploy) porque el proyecto ya las
tiene; se mantienen el build gate, la verificación por entrega, la supervisión, los
tests y un commit de git por tarea. Por defecto trabaja sobre el **directorio
actual** (`--output .`); apúntalo a otro con `--output <dir>`. Requiere que el
directorio ya contenga código (si está vacío, usa `/web` o `/app`).

Los commits van a una **rama dedicada** `multiagent/improve-<n>` — tu rama queda
intacta. La rama se guarda en `.multiagent/state.json`, así que `--resume`
continúa sobre **la misma rama** en vez de crear otra. Al terminar, el resumen
imprime la rama y los comandos exactos para revisarla (`git diff base...rama`),
integrarla o descartarla, junto con el coste estimado del run (si hay telemetría
activa) y la lista de tareas fallidas o bloqueadas por dependencias fallidas —
una tarea cuya dependencia falló ya no se construye sobre esa base rota: se
bloquea y se informa.

```
/improve migra los componentes de clase a hooks --output ./mi-app --approve
```

O usa el binario independiente:

```bash
# Solo plan (planificación completa, no se escribe código)
claw-multiagent web "un ecommerce para vender productos electrónicos" --dry-run

# Revisar el plan y confirmar antes de gastar (recomendado la primera vez)
claw-multiagent web "un ecommerce para vender productos electrónicos" \
  --output ./mi-tienda --approve --dashboard

# Build autónomo completo, 4 developers en paralelo
claw-multiagent web "un ecommerce para vender productos electrónicos" \
  --output ./mi-tienda --parallel 4

# Reanudar un build interrumpido (Ctrl+C guarda el estado; el segundo
# Ctrl+C fuerza la salida)
claw-multiagent web "..." --output ./mi-tienda --resume

# Apps: escritorio/móvil (Electron/Tauri/Flutter/React Native/MAUI/Kotlin/Swift)
claw-multiagent app "app de notas offline para Android y escritorio"
```

Flags principales (mismos nombres en `/web` y `/app` del REPL):

| Flag | Efecto |
| --- | --- |
| `--dry-run` | Solo planificación; guarda `docs/plan.json` y `docs/backlog.json` y para. |
| `--approve` | Pausa tras la planificación y pide confirmación (`s/N`) antes de construir. |
| `--parallel N` | Developers simultáneos (por defecto 4). |
| `--resume` | Reutiliza plan/diseños/backlog guardados y omite tareas completadas. |
| `--no-scaffold` | Sin plantilla determinista; los agentes generan todos los archivos. |
| `--build-cmd <cmd\|off>` | Comando del build gate (autodetectado si se omite). |
| `--max-cost-usd X` | Tope de gasto (APAGADO por defecto: las suscripciones no facturan por token). Requiere telemetría activa. |
| `--dashboard` | Arranca `claw-dashboard`, apunta la telemetría al build y abre el navegador. |

Los niveles de modelo asignan la complejidad de cada tarea a cualquier proveedor con credenciales configuradas
(Anthropic, OpenAI, xAI, DashScope, Ollama) — puedes sobrescribirlos por ejecución
(`--simple-model qwen-turbo --complex-model gpt-4o`) o de forma persistente en
`.claw/multiagent.json`. La ejecución falla rápido listando cualquier credencial que falte
(la autenticación guardada de una suscripción de Anthropic también cuenta).
El Director deja archivos de instrucciones por modelo (`CLAUDE.md`, `AGENTS.md`,
`GROK.md`) en el proyecto describiendo cada rol. Define `CLAW_DASHBOARD_EVENTS`
(o usa `--dashboard`) para observar cada agente en vivo, y
`CLAW_MULTIAGENT_REINDEX_CMD` para reindexar codebase-memory tras cada
entrega supervisada.

## Dashboard en vivo de tokens y agentes

`claw-dashboard` sirve una UI web local que muestra en vivo el uso de tokens de
entrada/salida/cache, el coste estimado y una tarjeta por cada sesión o agente activo (en ejecución,
completado, fallido), escalando a cualquier número de agentes en paralelo. Todo es
local: claw añade telemetría JSONL a un archivo y el dashboard lo sigue en cola —
ningún dato sale de la máquina.

La vía de un solo comando:

```bash
claw --dashboard
```

Esto inicia `claw-dashboard` en el puerto 4110 (o reutiliza uno en ejecución), apunta
`CLAW_DASHBOARD_EVENTS` a `.claw/telemetry/events.jsonl` y abre el
navegador. El binario del dashboard debe estar junto a `claw` (ambos lo están tras
`cargo build -p rusty-claude-cli -p claw-dashboard`).

Configuración manual, útil para muchos claws en paralelo compartiendo un dashboard:

```bash
# Terminal 1: inicia el dashboard (por defecto en el puerto 4110).
# --truncate descarta el historial de sesiones anteriores.
cd rust
cargo run -p claw-dashboard -- --events /tmp/claw-events.jsonl --truncate

# Terminal 2..N: ejecuta claw apuntando al mismo archivo de eventos, una etiqueta cada uno
CLAW_DASHBOARD_EVENTS=/tmp/claw-events.jsonl CLAW_AGENT_LABEL="migrate tests" claw
```

Abre http://127.0.0.1:4110. Cada proceso claw aparece como su propia tarjeta
(nombrada por `CLAW_AGENT_LABEL` cuando está definida), con el ciclo de vida
iniciado/terminado/fallido rastreado automáticamente. Las actualizaciones llegan por push SSE con un
fallback de polling. El uso se rastrea para Anthropic y para proveedores compatibles con OpenAI
(OpenAI, xAI, DashScope, Ollama).

## Verificación

```bash
cd rust
cargo test --workspace
```

## Visión general del workspace

Crates de Rust actuales:

- `api`
- `claw-analog`
- `claw-dashboard`
- `claw-multiagent`
- `claw-rag-service`
- `commands`
- `compat-harness`
- `mock-anthropic-service`
- `plugins`
- `runtime`
- `rusty-claude-cli`
- `telemetry`
- `tools`
