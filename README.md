# Claw Code

<p align="center">
  <a href="https://github.com/code-yeongyu/lazycodex">
    <img src="https://img.shields.io/badge/LazyCodex-codex%20for%20no--brainers-111111?style=for-the-badge&logo=github&logoColor=white" alt="LazyCodex banner" />
  </a>
  <a href="https://github.com/Yeachan-Heo/gajae-code">
    <img src="https://img.shields.io/badge/Gajae--Code-red--claw%20agent%20harness-B22222?style=for-the-badge&logo=github&logoColor=white" alt="Gajae-Code banner" />
  </a>
</p>

<p align="center">
  <a href="https://github.com/code-yeongyu/lazycodex">
    <img src="https://opengraph.githubassets.com/lazycodex-card/code-yeongyu/lazycodex" alt="LazyCodex GitHub card" width="280" />
  </a>
  <a href="https://github.com/Yeachan-Heo/gajae-code">
    <img src="https://opengraph.githubassets.com/gajae-code-card/Yeachan-Heo/gajae-code" alt="Gajae-Code GitHub card" width="280" />
  </a>
</p>

<h3 align="center">empieza con los harnesses cangrejiles de verdad</h3>

<p align="center">
  <a href="https://github.com/code-yeongyu/lazycodex"><b>github.com/code-yeongyu/lazycodex</b></a>
  <br/>
  <a href="https://github.com/Yeachan-Heo/gajae-code"><b>github.com/Yeachan-Heo/gajae-code</b></a>
</p>

<p align="center">
  <a href="https://github.com/code-yeongyu/lazycodex">
    <img src="https://img.shields.io/badge/Open-LazyCodex-111111?style=flat-square&logo=github&logoColor=white" alt="Abrir LazyCodex en GitHub" />
  </a>
  <a href="https://github.com/Yeachan-Heo/gajae-code">
    <img src="https://img.shields.io/badge/Open-Gajae--Code-B22222?style=flat-square&logo=github&logoColor=white" alt="Abrir Gajae-Code en GitHub" />
  </a>
</p>

<p align="center">
  <a href="https://discord.gg/GtjhvgjnV">
    <img src="https://img.shields.io/badge/Discord-join%20the%20harness%20lab-5865F2?style=for-the-badge&logo=discord&logoColor=white" alt="Únete al harness lab en Discord" />
  </a>
  <a href="https://discord.gg/4Rt79F7dF">
    <img src="https://img.shields.io/badge/Discord-join%20the%20crab%20tank-5865F2?style=for-the-badge&logo=discord&logoColor=white" alt="Únete al crab tank en Discord" />
  </a>
</p>

<p align="center">
  Únete a los Discords:
  <a href="https://discord.gg/GtjhvgjnV"><b>discord de ultraworkers</b></a>
  ·
  <a href="https://discord.gg/4Rt79F7dF"><b>discord de gajae-code</b></a>
</p>

> [!IMPORTANT]
> **Claw Code no es el proyecto serio de producción aquí.**
> Este repositorio se parece más a una pieza de museo que a un pitch de producto: un artefacto gestionado por crustáceos, mantenido con vida por gajaes con pinzas, barrido y etiquetado por agentes, y mantenido automáticamente según los harnesses de arriba.
>
> Como ya describe la filosofía del proyecto, esto no está pensado para operarse a mano como un repo de producto normal. Es una **exhibición gestionada por agentes**: los harnesses planifican, ejecutan, verifican, etiquetan y preservan el artefacto mientras los cangrejos mantienen el acuario en marcha.
>
> Si quieres trabajar de verdad, empieza con **[LazyCodex](https://github.com/code-yeongyu/lazycodex)** o **[Gajae-Code](https://github.com/Yeachan-Heo/gajae-code)**. Si quieres inspeccionar el pequeño y extraño fósil del momento Claw Code, sigue leyendo.
>
> Para la explicación pública más larga detrás de esta filosofía, mira [aquí](https://x.com/realsigridjin/status/2039472968624185713).

<p align="center">
  <a href="https://github.com/charcsllc/claw-code">charcsllc/claw-code</a>
  ·
  <a href="./USAGE.md">Uso</a>
  ·
  <a href="./rust/README.md">Workspace Rust</a>
  ·
  <a href="./PARITY.md">Parity</a>
  ·
  <a href="./ROADMAP.md">Roadmap</a>
  ·
  <a href="./CONTRIBUTING.md">Contribuir</a>
  ·
  <a href="./SECURITY.md">Seguridad</a>
  ·
  <a href="https://discord.gg/5TUQKqFWd">Discord de UltraWorkers</a>
</p>

<p align="center">
  <a href="https://star-history.com/#charcsllc/claw-code&Date">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=charcsllc/claw-code&type=Date&theme=dark" />
      <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=charcsllc/claw-code&type=Date" />
      <img alt="Historial de estrellas de charcsllc/claw-code" src="https://api.star-history.com/svg?repos=charcsllc/claw-code&type=Date" width="600" />
    </picture>
  </a>
</p>

<p align="center">
  <img src="assets/claw-hero.jpeg" alt="Claw Code" width="300" />
</p>

Claw Code es la implementación pública en Rust del harness de agente CLI `claw`.
La implementación canónica vive en [`rust/`](./rust), y la fuente de verdad actual de este repositorio es **charcsllc/claw-code**.

> [!IMPORTANT]
> Empieza por [`USAGE.md`](./USAGE.md) para los flujos de build, autenticación, CLI, sesiones y el harness de parity. Para dudas sobre envío/navegación de archivos, consulta [Navegación y contexto de archivos](./docs/navigation-file-context.md). Para modelos locales compatibles con OpenAI e instalación de skills sin conexión, mira [Proveedores locales compatibles con OpenAI y setup de skills](./docs/local-openai-compatible-providers.md). Los usuarios de Windows pueden saltar directamente al [quickstart de instalación y releases en Windows](./docs/windows-install-release.md), centrado en PowerShell. Haz de `claw doctor` tu primer chequeo de salud después de compilar, usa [`rust/README.md`](./rust/README.md) para el detalle por crate, lee [`PARITY.md`](./PARITY.md) para el checkpoint actual del port a Rust, y mira [`docs/container.md`](./docs/container.md) para el flujo container-first.
>
> **Estado de ACP / Zed:** `claw-code` todavía no incluye un daemon ACP/Zed ni un entrypoint JSON-RPC. Ejecuta `claw acp` (o `claw --acp`) para ver el estado actual en vez de adivinarlo por la estructura del código; `claw acp serve` es de momento solo un alias de descubrimiento, devuelve el estado con código de salida 0, y el soporte real de ACP se sigue trazando por separado en `ROADMAP.md`. Para el contrato JSON público, consulta [`docs/g011-acp-json-rpc-status-contract.md`](./docs/g011-acp-json-rpc-status-contract.md).

## Forma actual del repositorio

- **`rust/`** — workspace canónico de Rust y el binario CLI `claw`
- **`USAGE.md`** — guía de uso orientada a tareas para la superficie actual del producto
- **`PARITY.md`** — estado de parity del port a Rust y notas de migración
- **`ROADMAP.md`** — roadmap activo y backlog de limpieza
- **`PHILOSOPHY.md`** — intención del proyecto y encuadre de diseño del sistema
- **`src/` + `tests/`** — workspace complementario de Python/referencia y utilidades de auditoría; no es la superficie principal de ejecución

## Novedades destacadas

- **Plataforma multiagente (`/web` y `/app`)** — a partir de un solo prompt, una jerarquía de agentes (Director → Arquitectos → Subdirector → developers en paralelo → Supervisor → Técnico → QA → Docs) construye un proyecto web o una aplicación completa: scaffold determinista, contratos de tipos compartidos, scheduler por grafo sin barreras, verificación por entrega, build gate, tests reales, smoke test del servidor y un commit de git por tarea. Detalles y flags en [`USAGE.md`](./USAGE.md).
- **Dashboard local (`claw-dashboard`)** — telemetría en vivo en el navegador: tokens de entrada/salida por sesión, coste estimado, y el workflow multiagente con cada agente en ejecución/terminado en tiempo real. 100 % local (lee un JSONL; nada sale de tu máquina).
- **`claw mcp add` / `claw mcp remove`** — instalación de servidores MCP en un comando (stdio, HTTP, SSE), con validación y rollback seguro de la configuración.

## Inicio rápido

> [!NOTE]
> [!WARNING]
> **`cargo install claw-code` instala lo que no es.** El crate `claw-code` de crates.io es un stub deprecado que coloca `claw-code-deprecated.exe` — no `claw`. Al ejecutarlo solo imprime `"claw-code has been renamed to agent-code"`. **No uses `cargo install claw-code`.** O compila desde el código fuente (este repo) o instala el binario upstream:
> ```bash
> cargo install agent-code   # binario upstream — instala 'agent.exe' (Windows) / 'agent' (Unix), NO 'agent-code'
> ```
> Este repo (`charcsllc/claw-code`) es **solo build desde código fuente** — sigue los pasos de abajo.

```bash
# 1. Clona y compila
git clone https://github.com/charcsllc/claw-code
cd claw-code/rust
cargo build --workspace

# 2. Configura tu API key (API key de Anthropic — no una suscripción de Claude)
export ANTHROPIC_API_KEY="sk-ant-..."

# 3. Verifica que todo está bien conectado
./target/debug/claw doctor

# 4. Ejecuta un prompt
./target/debug/claw prompt "say hello"

# 5. Arranca una sesión interactiva
./target/debug/claw
```

> [!NOTE]
> **Windows (PowerShell):** el binario es `claw.exe`, no `claw`. Usa `.\target\debug\claw.exe` o ejecuta `cargo run -- prompt "say hello"` para saltarte la búsqueda de la ruta.

### Configuración en Windows

**PowerShell es una ruta soportada en Windows.** Usa la shell que mejor te funcione. Los problemas de onboarding más comunes en Windows son:

1. **Instala Rust primero** — descárgalo de <https://rustup.rs/> y ejecuta el instalador. Cierra y reabre la terminal cuando termine.
2. **Verifica que Rust está en el PATH:**
   ```powershell
   cargo --version
   ```
   Si falla, reabre la terminal o ejecuta el setup de PATH que indica la salida del instalador de Rust, y reintenta.
3. **Clona y compila** (funciona en PowerShell, Git Bash o WSL):
   ```powershell
   git clone https://github.com/charcsllc/claw-code
   cd claw-code/rust
   cargo build --workspace
   ```
4. **Ejecuta** (PowerShell — fíjate en el `.exe` y la barra invertida):
   ```powershell
   $env:ANTHROPIC_API_KEY = "sk-ant-..."
   .\target\debug\claw.exe prompt "say hello"
   ```

Para ZIPs de release, configuración del PATH, cambio de proveedor y pruebas de notificaciones, consulta [`docs/windows-install-release.md`](./docs/windows-install-release.md).

**Git Bash / WSL** son alternativas opcionales, no requisitos. Si prefieres rutas estilo bash (`/c/Users/tu/...` en vez de `C:\Users\tu\...`), Git Bash (viene con Git para Windows) funciona bien. En Git Bash, el prompt `MINGW64` es lo esperado y normal — no una instalación rota.

## Después del build: localiza el binario y verifica

Tras ejecutar `cargo build --workspace`, el binario `claw` queda compilado pero **no** se instala automáticamente en tu sistema. Aquí tienes dónde encontrarlo y cómo verificar que el build funcionó.

### Ubicación del binario

Después de `cargo build --workspace` en `claw-code/rust/`:

**Build de debug (por defecto, compila más rápido):**
- **macOS/Linux:** `rust/target/debug/claw`
- **Windows:** `rust/target/debug/claw.exe`

**Build de release (optimizado, compila más lento):**
- **macOS/Linux:** `rust/target/release/claw`
- **Windows:** `rust/target/release/claw.exe`

Si ejecutaste `cargo build` sin `--release`, el binario está en la carpeta `debug/`.

### Verifica que el build funcionó

Prueba el binario directamente usando su ruta:

```bash
# macOS/Linux (build de debug)
./rust/target/debug/claw --help
./rust/target/debug/claw doctor

# Windows PowerShell (build de debug)
.\rust\target\debug\claw.exe --help
.\rust\target\debug\claw.exe doctor
```

Comandos de smoke en PowerShell que no requieren credenciales reales:

```powershell
$env:CLAW_CONFIG_HOME = Join-Path $env:TEMP "claw config home"
New-Item -ItemType Directory -Force -Path $env:CLAW_CONFIG_HOME | Out-Null
Remove-Item Env:\ANTHROPIC_API_KEY, Env:\ANTHROPIC_AUTH_TOKEN, Env:\OPENAI_API_KEY -ErrorAction SilentlyContinue
.\rust\target\debug\claw.exe help
.\rust\target\debug\claw.exe status
.\rust\target\debug\claw.exe config env
.\rust\target\debug\claw.exe doctor
```

Si estos comandos funcionan, el build está bien. `claw doctor` es tu primer chequeo de salud — valida tu API key, el acceso a modelos y la configuración de herramientas.

### Opcional: añadir al PATH

Si quieres ejecutar `claw` desde cualquier directorio sin la ruta completa, elige una de estas opciones:

**Opción 1: Symlink (macOS/Linux)**
```bash
ln -s $(pwd)/rust/target/debug/claw /usr/local/bin/claw
```
Después recarga la shell y prueba:
```bash
claw --help
```

**Opción 2: Usa `cargo install` (todas las plataformas)**

Compila e instala en la ubicación por defecto de Cargo (`~/.cargo/bin/`, que normalmente está en el PATH):
```bash
# Desde el directorio claw-code/rust/
cargo install --path . --force

# Y luego, desde cualquier sitio
claw --help
```

**Opción 3: Actualiza el perfil de la shell (bash/zsh)**

Añade esta línea a `~/.bashrc` o `~/.zshrc`:
```bash
export PATH="$(pwd)/rust/target/debug:$PATH"
```

Recarga la shell:
```bash
source ~/.bashrc  # o source ~/.zshrc
claw --help
```

### Solución de problemas

- **"command not found: claw"** — El binario está en `rust/target/debug/claw`, pero no está en tu PATH. Usa la ruta completa `./rust/target/debug/claw` o haz symlink/instálalo como arriba.
- **"permission denied"** — En macOS/Linux puede que necesites `chmod +x rust/target/debug/claw` si el bit de ejecución no está activo (raro).
- **Debug vs. release** — Si el binario va lento, estás en modo debug (el default). Añade `--release` a `cargo build` para mejor rendimiento en ejecución, aunque el build tardará 5–10 minutos.

> [!NOTE]
> **Autenticación:** claw requiere una **API key** (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.) — el login con suscripción de Claude no es una ruta de autenticación soportada.

Ejecuta la suite de tests del workspace después de verificar que el binario funciona:

```bash
cd rust
cargo test --workspace
```

## Mapa de documentación

- [`USAGE.md`](./USAGE.md) — comandos rápidos, autenticación, sesiones, configuración, harness de parity, multiagente y dashboard
- [`docs/navigation-file-context.md`](./docs/navigation-file-context.md) — navegación en terminal, scrollback, contexto de archivos con `@ruta`, adjuntos y pautas de seguridad con secretos
- [`docs/local-openai-compatible-providers.md`](./docs/local-openai-compatible-providers.md) — setup de Ollama/llama.cpp/vLLM, posicionamiento multiproveedor de Claw y comprobaciones de instalación local de skills
- [`docs/windows-install-release.md`](./docs/windows-install-release.md) — instalación PowerShell-first, artefactos de release, cambio de proveedor y rutas de smoke de notificaciones en Windows/WSL
- [`rust/README.md`](./rust/README.md) — mapa de crates, superficie del CLI, features y layout del workspace
- [`PARITY.md`](./PARITY.md) — estado de parity del port a Rust
- [`rust/MOCK_PARITY_HARNESS.md`](./rust/MOCK_PARITY_HARNESS.md) — detalles del harness determinista con servicio mock
- [`ROADMAP.md`](./ROADMAP.md) — roadmap activo y trabajo de limpieza pendiente
- [`docs/g004-events-reports-contract.md`](./docs/g004-events-reports-contract.md) — guía del contrato de eventos/reportes de lanes (Stream 2) para consumidores
- [`PHILOSOPHY.md`](./PHILOSOPHY.md) — por qué existe el proyecto y cómo se opera
- [`CONTRIBUTING.md`](./CONTRIBUTING.md), [`SECURITY.md`](./SECURITY.md), [`SUPPORT.md`](./SUPPORT.md) y [`CODE_OF_CONDUCT.md`](./CODE_OF_CONDUCT.md) — políticas de contribución, reporte de vulnerabilidades, soporte y comunidad
- [`LICENSE`](./LICENSE) — licencia MIT de este repositorio

## Ecosistema

Claw Code se construye en abierto junto al resto del toolchain de UltraWorkers:

- [clawhip](https://github.com/Yeachan-Heo/clawhip)
- [oh-my-openagent](https://github.com/code-yeongyu/oh-my-openagent)
- [oh-my-claudecode](https://github.com/Yeachan-Heo/oh-my-claudecode)
- [oh-my-codex](https://github.com/Yeachan-Heo/oh-my-codex)
- [gajae-code](https://github.com/Yeachan-Heo/gajae-code)
- [Discord de UltraWorkers](https://discord.gg/5TUQKqFWd)

## Aviso de propiedad / afiliación

- Este repositorio **no** reclama la propiedad del material fuente original de Claude Code.
- Este repositorio **no está afiliado a, respaldado por, ni mantenido por Anthropic**.
