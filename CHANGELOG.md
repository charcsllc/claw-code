# Changelog

Los cambios notables de este proyecto se documentan en este archivo.

El formato está adaptado de [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/).
El proyecto todavía no corta versiones semánticas: las entradas se agrupan por
las rondas de mejora que aterrizan en `main`. Repositorio canónico:
[github.com/charcsllc/claw-code](https://github.com/charcsllc/claw-code).

## [Unreleased]

### Added
- `rust/rust-toolchain.toml` fija el toolchain a **1.96.1** (clippy + rustfmt), y los workflows de CI usan la misma versión pineada — se acabó el clippy de CI más nuevo que el local rompiendo el build.
- Hook `pre-push` en `.githooks/` que replica los gates de CI (rustfmt, checkers de docs, clippy); se activa con `git config core.hooksPath .githooks`.
- Workflow `lint.yml` con **shellcheck** sobre `install.sh`, `scripts/*.sh` y `.githooks/*`.
- Job de CI que compila el CLI y ejecuta `scripts/bench-startup.sh` como gate de regresión de arranque.
- Gate de **release readiness** en `release.yml`: los checkers de docs deben pasar antes de compilar y subir binarios de release.
- El checker `check_usage_commands.py` corre ahora también en CI, y ~10 comandos verificados (`/stats`, `/tokens`, `/cache`, `/version`, `/clear`, `/resume`, `/sandbox`, `/memory`, `/init`, `/agents`) salen del baseline y entran en `USAGE.md`.

### Changed
- Cache de cargo (`Swatinem/rust-cache`) también en el workflow `rust.yml`.
- Los binarios de release de macOS se compilan en `macos-latest`.

## Rondas recientes

### `4571d31` — fix: assert! sin referencia redundante
- Elimina una referencia redundante en un argumento de formato de `assert!`.
- Limpieza para mantener clippy en verde con `-D warnings`.

### `473e111` — docs: ancla local rota
- Corrige el ancla local a la sección del endpoint compatible con OpenAI en la documentación.
- Mantiene el checker de enlaces locales (`check_release_readiness.py`) en verde.

### `10ac842` — feat: ronda de 30 mejoras
- Nuevos comandos `/tasks`, `/retry` y `/diff <archivo>`.
- `/design-review fix` y modo CI del gate de diseño (`--output-format json`, exit 1 con hallazgos).
- Pipeline E2E multiagente contra un mock del API (cero tokens reales).
- Desinstaladores: `./install.sh --uninstall` e `.\install.ps1 -Uninstall`.

### `cab49f7` — feat: /design-review y revisión contra base
- Comando `/design-review` aplicable a cualquier proyecto.
- `/review <rama-base>` revisa todo lo que tu rama añade sobre la base.
- Soporte de bun por lockfile, preset de groq y salida de bash tunable.

### `c66b768` — feat: subsistema de diseño, ronda 2
- Tres arquetipos más de producto para la dirección de arte.
- `base.css` de suelo de accesibilidad junto a los tokens.
- Disciplina de tokens (detección de hex hardcodeados) y gate categorizado (`[CONTRASTE]`/`[A11Y]`/`[TOKENS]`).

### `fdd0835` — feat: subsistema de diseño
- Dirección de arte por arquetipo detectado del prompt.
- Fundación de tokens validada que cumple WCAG AA por construcción.
- Gate de diseño determinista de 0 tokens (contraste WCAG + auditoría del HTML).

### `9b5812c` — feat: mega-ronda paralela
- Alias de modelo y pistas de error más ricas.
- Listas de tareas markdown en el renderizado.
- Gates de build para pnpm/yarn y presupuestos ajustables por variables de entorno.

### `5a74e2a` — feat: ronda de calidad de vida
- Pistas accionables en los errores (401, 429, contexto lleno, red).
- Métricas de caché de prompts en `/usage` y `/color` persistente entre sesiones.
- Preset de OpenRouter y escaneos más seguros.

### `a53d8f6` — feat: ronda de 30 más
- Nuevos comandos `/plan`, `/review`, `/fast` y `/security-review`.
- Builds multiagente que fallan rápido sin credenciales.
- Errores de edición más inteligentes (sugerencia de dónde quedó el ancla).

### `851aaaa` — feat: ronda de 30 mejoras
- Cinco comandos más en vivo en el REPL.
- Endurecimiento de seguridad (permisos de archivos de sesión y settings).
- Pulido del pipeline multiagente.

### `1de76ac` — feat: seis comandos REPL en vivo
- Seis comandos interactivos nuevos y fix de persistencia de `/effort`.
- Auto-sondeo del proveedor al arrancar.
- `/context`, `/doctor` y el digest del repo más ricos.

### `1ec06bb` — feat: visibilidad de reintentos
- Reintentos visibles en vivo durante el turno (`⟳ reintento k/9`).
- `/provider test` con sondeo real de 1 token y nuevo `/upgrade`.

### `af04120` — feat: resume de improve-branch y reporting
- `--resume` continúa `/improve` sobre la misma rama dedicada.
- Reporte de coste y fallos por ejecución; nuevos `/usage` y `/context`.
- El proveedor activo aparece en `/status`.

### `6776ce4` — feat: presets de /provider
- Presets de autenticación en un comando (`/provider use <preset> <clave>`).
- El proveedor guardado se aplica al arrancar la sesión.
- Salvaguardas de rama para `/improve`.

### `9a7b58f` — fix: /improve no pisa tus archivos
- `/improve` deja de poder sobrescribir archivos del usuario.
- Un target rechazado ya no ensucia el árbol de trabajo.

### `661a5cc` — feat: /improve
- El pipeline multiagente aplicado a proyectos EXISTENTES: analiza el repo, planifica solo el cambio pedido y lo implementa respetando el stack.
- Commits en una rama dedicada `multiagent/improve-<n>`.

### `1cdc14b` — fix: instrucciones de autenticación
- Actualiza las instrucciones de API key y token en los scripts de instalación.

### `62ae635` — feat: instalación y docs de Windows
- Mejora el proceso de instalación y la documentación para usuarios de Windows.
- PowerShell como ruta soportada de primera clase.

### `494d8fe` — fix: bugs de la capa de terminal
- Resiliencia del REPL y manejo correcto de Ctrl+C.
- Enmascarado de secretos también al persistir la sesión en disco.
- Correcciones de renderizado.

### `39e5157` — feature
- Ronda inicial de funcionalidad previa a las rondas etiquetadas.
