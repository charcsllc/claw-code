//! Design subsystem for the multiagent pipeline: the graphic-design agent's
//! brief, a validated design-token foundation written as real CSS, and a
//! deterministic (zero-token) design gate that verifies WCAG contrast and
//! rendered-HTML accessibility after the build.
//!
//! Philosophy (same as the scaffold): the parts of visual design that are
//! computable — contrast ratios, token scales, a CVD-safe categorical order —
//! are computed and validated here, not delegated to an LLM's taste. The
//! design agent then spends its tokens on what it is actually good at:
//! composition, hierarchy and product-specific art direction on top of a
//! foundation that is accessible by construction.
//!
//! The palette values come from a validated reference set: every text/surface
//! pair below meets WCAG AA on its surface (the unit tests recompute the
//! ratios with the same math the gate uses), and the eight data slots are
//! ordered to maximize the minimum adjacent color-vision-deficiency distance
//! — the order IS the accessibility mechanism, so it must never be re-sorted.

use std::path::{Path, PathBuf};

use crate::contracts::Plan;

// ---------- Product archetype ----------

/// What kind of product is being designed. Each archetype carries its own
/// art direction: a dashboard and a landing page that look alike are both
/// wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignArchetype {
    /// Marketing/landing: hero-driven, expressive, conversion-focused.
    Landing,
    /// Store: product-first imagery, trust signals, frictionless checkout.
    Ecommerce,
    /// Data-dense tool: neutral surfaces, tables/charts first, low chrome.
    Dashboard,
    /// SaaS product app: task flows, forms, onboarding, empty states.
    Saas,
    /// Editorial/blog/docs: typography-first, long-form reading comfort.
    Content,
    /// Desktop/mobile app or anything that matched no other archetype.
    General,
}

impl DesignArchetype {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Landing => "landing",
            Self::Ecommerce => "ecommerce",
            Self::Dashboard => "dashboard",
            Self::Saas => "saas",
            Self::Content => "contenido editorial",
            Self::General => "general",
        }
    }

    /// Art direction the archetype demands — injected into both the
    /// planning brief and the build-phase prompt.
    #[must_use]
    pub const fn direction(self) -> &'static str {
        match self {
            Self::Landing => {
                "Art direction (landing): one dominant hero with a single primary CTA \
                 above the fold; generous whitespace (section padding >= var(--space-16)); \
                 expressive display headings (--text-3xl+, tight leading); social proof \
                 (logos/testimonials) as a distinct section; ONE accent color doing all \
                 the emphasis work; scroll rhythm alternating dense/airy sections; \
                 footer with real sitemap links. Avoid: three competing CTAs, carousels, \
                 fake urgency banners."
            }
            Self::Ecommerce => {
                "Art direction (ecommerce): product imagery is the interface — cards with \
                 consistent aspect-ratio image slots (use aspect-ratio CSS, object-fit: \
                 cover, and a --surface-sunken placeholder while loading); price always \
                 --text-primary and larger than the product name; trust signals (payment \
                 icons, return policy) near every buy action; cart state visible in the \
                 navbar with a count badge; filters as accessible controls, not divs. \
                 Avoid: walls of identical gray cards, buy buttons below the fold on \
                 product pages."
            }
            Self::Dashboard => {
                "Art direction (dashboard): density over decoration — neutral surfaces, \
                 hairline borders (--border-hairline), NO heavy card shadows; numbers \
                 right-aligned in tables with tabular-nums; use the --data-1..--data-8 \
                 slots for series IN ORDER (the order is CVD-safe; never re-sort it); \
                 charts never encode meaning by color alone (add labels/patterns); every \
                 metric names its unit and period; empty and loading states designed for \
                 every widget (skeletons, not spinners). Avoid: rainbow KPI cards, pie \
                 charts for more than 4 slices, dual-axis charts."
            }
            Self::Saas => {
                "Art direction (SaaS app): task flows first — forms with visible labels \
                 (never placeholder-as-label), inline validation on blur, primary action \
                 bottom-right of the form; onboarding empty states that teach (icon + one \
                 sentence + CTA); persistent left nav or topbar with the current section \
                 highlighted via more than color (weight/underline/background); \
                 destructive actions behind confirmation, styled --status-critical. \
                 Avoid: modal-on-modal, unsaved-changes traps, disabled buttons with no \
                 explanation."
            }
            Self::Content => {
                "Art direction (editorial): typography is the design — measure 60-75ch \
                 (max-width on prose containers), --leading-relaxed for body, a real \
                 typographic hierarchy (--text-3xl/2xl/xl for h1/h2/h3, never bold-only); \
                 dark mode is a reading feature, verify long-form contrast in both; \
                 images with captions and mandatory alt text; a visible table of contents \
                 for long pages. Avoid: full-width unreadable text lines, justified text, \
                 more than two typefaces."
            }
            Self::General => {
                "Art direction (general): platform-native feel — respect OS conventions \
                 for spacing and controls; a clear visual hierarchy per screen (one \
                 primary action, obviously primary); consistent iconography from ONE set; \
                 keyboard operability for every control (visible :focus-visible ring); \
                 responsive from 320px up. Avoid: mystery-meat icon-only actions without \
                 labels or tooltips."
            }
        }
    }
}

/// Keyword-based archetype detection over the Director's plan. Deliberately
/// simple: vision + scope + epic names, first match in priority order
/// (ecommerce beats landing when both appear — the store IS the product).
#[must_use]
pub fn detect_archetype(plan: &Plan) -> DesignArchetype {
    let mut haystack = plan.vision.to_lowercase();
    for item in &plan.scope {
        haystack.push(' ');
        haystack.push_str(&item.to_lowercase());
    }
    for epic in &plan.epics {
        haystack.push(' ');
        haystack.push_str(&epic.name.to_lowercase());
    }
    let matches_any = |needles: &[&str]| needles.iter().any(|needle| haystack.contains(needle));

    if matches_any(&[
        "ecommerce",
        "e-commerce",
        "tienda",
        "carrito",
        "checkout",
        "shop",
        "store",
        "vender",
        "productos",
        "catálogo",
        "catalogo",
    ]) {
        DesignArchetype::Ecommerce
    } else if matches_any(&[
        "dashboard",
        "panel",
        "analytics",
        "analítica",
        "métricas",
        "metricas",
        "admin",
        "monitoring",
        "monitoreo",
        "kpi",
        "reporting",
        "informes",
    ]) {
        DesignArchetype::Dashboard
    } else if matches_any(&[
        "landing",
        "página de aterrizaje",
        "marketing",
        "portfolio",
        "portafolio",
        "promocion",
        "promoción",
        "presentación de producto",
    ]) {
        DesignArchetype::Landing
    } else if matches_any(&[
        "blog",
        "documentación",
        "documentacion",
        "docs",
        "revista",
        "noticias",
        "artículos",
        "articulos",
        "cms",
        "editorial",
    ]) {
        DesignArchetype::Content
    } else if matches_any(&[
        "saas",
        "suscripción",
        "suscripcion",
        "crm",
        "erp",
        "gestión",
        "gestion",
        "plataforma",
        "workspace",
        "colaboración",
        "colaboracion",
    ]) {
        DesignArchetype::Saas
    } else {
        DesignArchetype::General
    }
}

// ---------- Design tokens (validated foundation) ----------

/// The variables the deterministic gate audits, per theme:
/// (foreground, background, minimum WCAG ratio).
const CONTRAST_PAIRS: &[(&str, &str, f64)] = &[
    ("--text-primary", "--surface-page", 4.5),
    ("--text-primary", "--surface-raised", 4.5),
    ("--text-secondary", "--surface-page", 4.5),
    ("--text-muted", "--surface-page", 3.0), // large text / secondary UI
    ("--color-primary-contrast", "--color-primary", 4.5),
    ("--color-primary", "--surface-page", 3.0), // links / focus ring visibility
];

const LIGHT_TOKENS: &str = "  color-scheme: light dark;\n\
  /* --- surfaces --- */\n\
  --surface-page: #f9f9f7;\n\
  --surface-raised: #fcfcfb;\n\
  --surface-sunken: #f0efec;\n\
  --border-hairline: #e1e0d9;\n\
  --border-strong: #c3c2b7;\n\
  /* --- ink --- */\n\
  --text-primary: #0b0b0b;\n\
  --text-secondary: #52514e;\n\
  --text-muted: #898781;\n\
  --text-invert: #ffffff;\n\
  /* --- brand --- */\n\
  --color-primary: #256abf;\n\
  --color-primary-hover: #1c5cab;\n\
  --color-primary-active: #184f95;\n\
  --color-primary-contrast: #ffffff;\n\
  --color-primary-soft: #cde2fb;\n\
  --color-accent: #1baf7a;\n\
  /* --- status: ALWAYS paired with an icon + label, never color alone --- */\n\
  --status-good: #0ca30c;\n\
  --status-warning: #fab219;\n\
  --status-serious: #ec835a;\n\
  --status-critical: #d03b3b;\n\
  /* --- data slots: CVD-safe ORDER, assign in sequence, never re-sort --- */\n\
  --data-1: #2a78d6;\n\
  --data-2: #1baf7a;\n\
  --data-3: #eda100;\n\
  --data-4: #008300;\n\
  --data-5: #4a3aa7;\n\
  --data-6: #e34948;\n\
  --data-7: #e87ba4;\n\
  --data-8: #eb6834;\n\
  /* --- typography: 1.25 modular scale --- */\n\
  --font-sans: system-ui, -apple-system, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;\n\
  --font-mono: ui-monospace, 'Cascadia Code', 'Source Code Pro', Menlo, Consolas, monospace;\n\
  --text-xs: 0.75rem;\n\
  --text-sm: 0.875rem;\n\
  --text-base: 1rem;\n\
  --text-lg: 1.25rem;\n\
  --text-xl: 1.5625rem;\n\
  --text-2xl: 1.953rem;\n\
  --text-3xl: 2.441rem;\n\
  --font-regular: 400;\n\
  --font-medium: 500;\n\
  --font-semibold: 600;\n\
  --font-bold: 700;\n\
  --leading-tight: 1.2;\n\
  --leading-normal: 1.5;\n\
  --leading-relaxed: 1.7;\n\
  /* --- spacing: 4px grid --- */\n\
  --space-1: 0.25rem;\n\
  --space-2: 0.5rem;\n\
  --space-3: 0.75rem;\n\
  --space-4: 1rem;\n\
  --space-6: 1.5rem;\n\
  --space-8: 2rem;\n\
  --space-12: 3rem;\n\
  --space-16: 4rem;\n\
  /* --- radii --- */\n\
  --radius-sm: 4px;\n\
  --radius-md: 8px;\n\
  --radius-lg: 16px;\n\
  --radius-full: 9999px;\n\
  /* --- elevation --- */\n\
  --shadow-1: 0 1px 2px rgba(11, 11, 11, 0.06);\n\
  --shadow-2: 0 2px 8px rgba(11, 11, 11, 0.10);\n\
  --shadow-3: 0 8px 24px rgba(11, 11, 11, 0.14);\n\
  /* --- z-index scale --- */\n\
  --z-nav: 100;\n\
  --z-dropdown: 200;\n\
  --z-modal: 300;\n\
  --z-toast: 400;\n\
  /* --- motion --- */\n\
  --duration-fast: 150ms;\n\
  --duration-base: 250ms;\n\
  --ease-out: cubic-bezier(0.2, 0, 0, 1);\n\
  /* --- focus (keyboard visibility is non-negotiable) --- */\n\
  --focus-ring: 2px solid var(--color-primary);\n\
  --focus-offset: 2px;\n";

const DARK_TOKENS: &str = "  --surface-page: #0d0d0d;\n\
  --surface-raised: #1a1a19;\n\
  --surface-sunken: #141413;\n\
  --border-hairline: #2c2c2a;\n\
  --border-strong: #383835;\n\
  --text-primary: #ffffff;\n\
  --text-secondary: #c3c2b7;\n\
  --text-muted: #898781;\n\
  --text-invert: #0b0b0b;\n\
  --color-primary: #3987e5;\n\
  --color-primary-hover: #5598e7;\n\
  --color-primary-active: #6da7ec;\n\
  --color-primary-contrast: #0b0b0b;\n\
  --color-primary-soft: #104281;\n\
  --color-accent: #199e70;\n\
  --data-1: #3987e5;\n\
  --data-2: #199e70;\n\
  --data-3: #c98500;\n\
  --data-4: #008300;\n\
  --data-5: #9085e9;\n\
  --data-6: #e66767;\n\
  --data-7: #d55181;\n\
  --data-8: #d95926;\n\
  --shadow-1: 0 1px 2px rgba(0, 0, 0, 0.4);\n\
  --shadow-2: 0 2px 8px rgba(0, 0, 0, 0.5);\n\
  --shadow-3: 0 8px 24px rgba(0, 0, 0, 0.6);\n";

/// The complete tokens stylesheet: light theme in `:root`, dark theme both
/// by explicit opt-in (`[data-theme="dark"]`) and by OS preference, plus
/// reduced-motion support baked in.
#[must_use]
pub fn design_tokens_css() -> String {
    format!(
        "/* Design tokens — accessible-by-construction foundation.\n\
         \x20  Generated by claw multiagent; every text/surface pair meets WCAG AA\n\
         \x20  (verified by the deterministic design gate). Components MUST consume\n\
         \x20  these variables — hardcoded colors fail review. Data slots --data-1..8\n\
         \x20  are ordered for color-vision safety: assign in order, never re-sort. */\n\
         :root {{\n{LIGHT_TOKENS}}}\n\n\
         [data-theme=\"dark\"] {{\n{DARK_TOKENS}}}\n\n\
         @media (prefers-color-scheme: dark) {{\n\
         \x20 :root:not([data-theme=\"light\"]) {{\n{DARK_TOKENS}  }}\n\
         }}\n\n\
         @media (prefers-reduced-motion: reduce) {{\n\
         \x20 :root {{\n    --duration-fast: 0ms;\n    --duration-base: 0ms;\n  }}\n\
         }}\n"
    )
}

/// Writes the validated token foundation into the project (skipped when any
/// stylesheet already defines `--surface-page` — resumed builds and projects
/// with their own tokens are left alone). Returns the relative path written.
pub fn write_design_tokens(project_dir: &Path) -> Option<String> {
    if !find_token_stylesheets(project_dir).is_empty() {
        return None;
    }
    let relative = "src/styles/design-tokens.css";
    let target = project_dir.join(relative);
    std::fs::create_dir_all(target.parent()?).ok()?;
    std::fs::write(&target, design_tokens_css()).ok()?;
    Some(relative.to_string())
}

// ---------- Prompts (planning brief, build phase, visual QA) ----------

/// Extra planning instructions for the UX/UI designer's architect run —
/// the generic "deliver your design document" prompt gave a graphic
/// designer nothing to push against.
#[must_use]
pub fn designer_planning_brief(plan: &Plan) -> String {
    let archetype = detect_archetype(plan);
    format!(
        "\n\nYou are the GRAPHIC DESIGN authority for this build. Product archetype \
         detected: {label}. {direction}\n\n\
         Your design document MUST specify, concretely (values, not adjectives):\n\
         1. Brand personality in 3 adjectives and how each maps to a visual decision.\n\
         2. Layout system: grid, breakpoints (320/768/1024/1440), container widths, \
         page templates for every epic in the plan.\n\
         3. Typographic hierarchy: exact token per level (--text-3xl..--text-xs), \
         weight and leading; where display type is allowed.\n\
         4. Color usage map: which token is used for what (primary = actions ONLY; \
         status colors NEVER for decoration; data slots in order for charts).\n\
         5. Component inventory the developers will need, each with its states \
         (default/hover/focus-visible/active/disabled/loading/error/empty).\n\
         6. Motion: which interactions animate (--duration-fast for feedback, \
         --duration-base for surfaces), and what NEVER animates.\n\
         7. Accessibility commitments: WCAG AA contrast, visible focus, touch \
         targets >= 44px, forms with visible labels, reduced-motion behavior.\n\
         8. Dark mode: which surfaces invert, what stays constant, imagery treatment.\n\
         A validated token foundation (design-tokens.css) already exists — design \
         WITH it; propose changes to tokens explicitly if the brand needs them, \
         never ad-hoc values.",
        label = archetype.label(),
        direction = archetype.direction(),
    )
}

/// The build-phase prompt for the design-system agent: turn the token
/// foundation + the designer's document into real, accessible components.
#[must_use]
pub fn design_system_prompt(archetype: DesignArchetype, tokens_path: &str) -> String {
    format!(
        "Read docs/plan.json and the UX/UI design document under docs/. Product \
         archetype: {label}. {direction}\n\n\
         A VALIDATED token foundation already exists at `{tokens_path}` (surfaces, \
         ink, brand, status, 8 CVD-safe data slots, type scale, spacing, radii, \
         shadows, z-index, motion, focus ring — light AND dark). Every text/surface \
         pair in it meets WCAG AA. Build on it NOW:\n\n\
         1. WIRE IT: import the tokens stylesheet globally (main stylesheet or \
         Tailwind @theme mapping if tailwindcss is in devDependencies). Add a \
         `data-theme` toggle (persisted to localStorage, defaulting to the OS \
         preference) in the app shell.\n\
         2. BASE STYLES: body uses --surface-page/--text-primary/--font-sans/\
         --leading-normal; headings use the scale; `:focus-visible` uses \
         --focus-ring + --focus-offset on EVERY interactive element; selection \
         color; ::placeholder uses --text-muted.\n\
         3. COMPONENTS under src/components/ui/ (or the stack's convention), each \
         consuming ONLY tokens (a hardcoded hex/px is a review failure), each with \
         default/hover/focus-visible/active/disabled states:\n\
         - Button: primary (--color-primary + --color-primary-contrast), secondary \
         (outline, --border-strong), ghost, destructive (--status-critical); \
         loading state with inline spinner; min touch target 44px.\n\
         - Input + Label + FieldError: visible label ALWAYS (placeholder is not a \
         label), error state uses --status-critical border + message + \
         aria-invalid/aria-describedby.\n\
         - Card: --surface-raised, --radius-md, --shadow-1, hairline border.\n\
         - Modal: --z-modal, focus trap, Escape closes, aria-modal, backdrop click.\n\
         - Toast: --z-toast, status-colored left border + icon + label, \
         auto-dismiss with pause-on-hover, aria-live=polite.\n\
         - Badge, Skeleton (animated with --duration-base, honors reduced-motion), \
         Spinner (with aria-label), EmptyState (icon + one sentence + CTA), \
         Navbar (current section marked by MORE than color), Footer, Table \
         (header --text-secondary, zebra --surface-sunken, numeric cells \
         right-aligned with font-variant-numeric: tabular-nums).\n\
         4. DOCUMENT: write docs/design-system.md — every token with its purpose, \
         every component with a usage snippet and its states, the dark-mode toggle \
         contract, and the rule that developers MUST use these components.\n\
         5. VERIFY: the project still builds; both themes render; keyboard-tab \
         reaches every control with a visible ring.",
        label = archetype.label(),
        direction = archetype.direction(),
    )
}

/// Visual-QA rubric: a concrete checklist beats "review the UI".
#[must_use]
pub fn visual_qa_prompt() -> String {
    "docs/rendered-dom.html is the homepage DOM exactly as rendered after \
     JavaScript ran (docs/screenshots/ may hold PNGs). Audit it against this \
     rubric together with the UI source, FIX every failure directly in the \
     repository, and write docs/visual-qa.md with a pass/fail per item:\n\
     1. Hierarchy: exactly one h1; heading levels don't skip; the primary action \
     of the page is visually dominant and unique.\n\
     2. Real content: no lorem ipsum, no empty sections, no placeholder images, \
     no {{unrendered}} template artifacts, no 'undefined'/'NaN' text.\n\
     3. Spacing rhythm: section padding and gaps come from the spacing scale — \
     no visually cramped (<8px) or accidental (>128px) gaps between siblings.\n\
     4. Tokens discipline: computed styles use the CSS variables; flag any \
     hardcoded hex colors in component styles.\n\
     5. States: every button/link has hover and :focus-visible styles; forms \
     show labels; disabled controls look disabled.\n\
     6. Accessibility: landmarks (header/main/footer), img alt text, form \
     label/aria wiring, touch targets >= 44px, contrast of any custom colors.\n\
     7. Dark mode: toggle (or prefers-color-scheme) actually swaps surfaces and \
     ink; nothing stays light-on-light or dark-on-dark.\n\
     8. Responsive: at 320px nothing overflows horizontally; nav collapses; \
     tables scroll inside their container, not the page.\n\
     9. Dead UI: no href=\"#\" links, no buttons that do nothing, no console \
     errors visible in the DOM (error boundaries triggered).\n\
     10. Feedback: async actions show loading state; failures surface a Toast \
     or inline error, never silence."
        .to_string()
}

// ---------- Deterministic design gate (zero tokens) ----------

/// WCAG relative luminance of an sRGB color.
fn relative_luminance(rgb: [u8; 3]) -> f64 {
    let linear = |channel: u8| {
        let c = f64::from(channel) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

/// WCAG contrast ratio between two colors (order-independent, 1.0..=21.0).
#[must_use]
pub fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (light, dark) = if la >= lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

fn parse_hex(value: &str) -> Option<[u8; 3]> {
    let hex = value.trim().strip_prefix('#')?;
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some([r, g, b])
        }
        3 => {
            let channel = |i: usize| {
                u8::from_str_radix(&hex[i..=i], 16)
                    .ok()
                    .map(|value| value * 17)
            };
            Some([channel(0)?, channel(1)?, channel(2)?])
        }
        _ => None,
    }
}

/// Extracts `--name: #hex` custom properties from a stylesheet into light
/// and dark maps. Everything before the first dark-theme scope marker is
/// light; later definitions override into the dark map (which falls back to
/// the light value for variables the dark theme doesn't redefine).
fn parse_theme_vars(
    css: &str,
) -> (
    std::collections::BTreeMap<String, [u8; 3]>,
    std::collections::BTreeMap<String, [u8; 3]>,
) {
    let mut light = std::collections::BTreeMap::new();
    let mut dark = std::collections::BTreeMap::new();
    let dark_marker = css
        .find("data-theme=\"dark\"")
        .or_else(|| css.find("prefers-color-scheme: dark"))
        .unwrap_or(css.len());
    for (offset, line) in css.lines().scan(0_usize, |pos, line| {
        let start = *pos;
        *pos += line.len() + 1;
        Some((start, line))
    }) {
        let Some((name_part, value_part)) = line.split_once(':') else {
            continue;
        };
        let name = name_part.trim();
        if !name.starts_with("--") {
            continue;
        }
        let value = value_part.trim().trim_end_matches(';');
        let Some(rgb) = parse_hex(value) else {
            continue;
        };
        if offset < dark_marker {
            light.insert(name.to_string(), rgb);
        } else {
            dark.insert(name.to_string(), rgb);
        }
    }
    // Dark falls back to light for anything not redefined.
    for (name, rgb) in &light {
        dark.entry(name.clone()).or_insert(*rgb);
    }
    (light, dark)
}

/// Audits a tokens stylesheet: every pair in [`CONTRAST_PAIRS`] must meet
/// its minimum in BOTH themes. Returns human-readable findings.
#[must_use]
pub fn audit_token_contrast(css: &str) -> Vec<String> {
    let (light, dark) = parse_theme_vars(css);
    let mut findings = Vec::new();
    for (theme_name, vars) in [("light", &light), ("dark", &dark)] {
        for (fg_name, bg_name, minimum) in CONTRAST_PAIRS {
            let (Some(fg), Some(bg)) = (vars.get(*fg_name), vars.get(*bg_name)) else {
                continue; // pair not defined in this stylesheet — not a failure
            };
            let ratio = contrast_ratio(*fg, *bg);
            if ratio < *minimum {
                findings.push(format!(
                    "contrast {theme_name}: {fg_name} on {bg_name} is {ratio:.2}:1 \
                     (needs >= {minimum}:1) — WCAG AA failure"
                ));
            }
        }
    }
    findings
}

/// Stylesheets under the project that look like a token foundation.
fn find_token_stylesheets(project_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![project_dir.to_path_buf()];
    let mut budget = 2_000_usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 || found.len() >= 4 {
                return found;
            }
            budget -= 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if !crate::orchestrator::REPO_SKIP_DIRS.contains(&name.as_str()) {
                    pending.push(path);
                }
                continue;
            }
            if name.ends_with(".css") && !name.ends_with(".min.css") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if content.contains("--surface-page") || content.contains("--color-primary") {
                        found.push(path);
                    }
                }
            }
        }
    }
    found
}

/// Deterministic accessibility audit of the rendered homepage DOM. Cheap
/// string checks by design — a real DOM parser would be sturdier but this
/// catches the failures that actually ship, with zero dependencies.
#[must_use]
pub fn audit_rendered_html(html: &str) -> Vec<String> {
    let lower = html.to_lowercase();
    let mut findings = Vec::new();
    if !lower.contains("<html lang") && !lower.contains("<html data-theme") {
        // data-theme without lang still fails; check lang specifically.
        if !lower.contains(" lang=") {
            findings.push("missing <html lang=\"…\"> — screen readers guess the language".into());
        }
    }
    if !lower.contains("name=\"viewport\"") && !lower.contains("name='viewport'") {
        findings.push("missing <meta name=\"viewport\"> — mobile renders at desktop width".into());
    }
    let img_count = lower.matches("<img").count();
    let img_with_alt = lower.matches("alt=").count();
    if img_count > img_with_alt {
        findings.push(format!(
            "{} <img> without alt attribute (of {img_count})",
            img_count - img_with_alt
        ));
    }
    let h1_count = lower.matches("<h1").count();
    if h1_count == 0 {
        findings.push("no <h1> — the page has no accessible title/hierarchy root".into());
    } else if h1_count > 1 {
        findings.push(format!(
            "{h1_count} <h1> elements — hierarchy needs exactly one"
        ));
    }
    if !lower.contains("<main") {
        findings.push("missing <main> landmark".into());
    }
    let dead_links = lower.matches("href=\"#\"").count() + lower.matches("href='#'").count();
    if dead_links > 0 {
        findings.push(format!("{dead_links} dead link(s) with href=\"#\""));
    }
    if lower.contains("lorem ipsum") {
        findings.push("placeholder 'lorem ipsum' text shipped to the rendered page".into());
    }
    if lower.contains(">undefined<") || lower.contains(">nan<") {
        findings.push("rendered output contains 'undefined'/'NaN' text nodes".into());
    }
    if !lower.contains("<title>") || lower.contains("<title></title>") {
        findings.push("missing or empty <title>".into());
    }
    findings
}

/// The whole gate: token contrast + rendered-DOM accessibility. Returns
/// findings; the orchestrator decides whether to dispatch the Fixer.
#[must_use]
pub fn run_design_gate(project_dir: &Path, docs: &Path) -> Vec<String> {
    let mut findings = Vec::new();
    let stylesheets = find_token_stylesheets(project_dir);
    if stylesheets.is_empty() {
        findings.push(
            "no design-token stylesheet found (expected CSS defining --surface-page / \
             --color-primary) — components have no shared foundation"
                .to_string(),
        );
    }
    for path in &stylesheets {
        if let Ok(css) = std::fs::read_to_string(path) {
            for finding in audit_token_contrast(&css) {
                findings.push(format!("{}: {finding}", path.display()));
            }
        }
    }
    if let Ok(html) = std::fs::read_to_string(docs.join("rendered-dom.html")) {
        for finding in audit_rendered_html(&html) {
            findings.push(format!("rendered-dom.html: {finding}"));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_math_matches_wcag_reference_points() {
        // Black on white is the canonical 21:1.
        let ratio = contrast_ratio([0, 0, 0], [255, 255, 255]);
        assert!((ratio - 21.0).abs() < 0.01, "got {ratio}");
        // Same color is 1:1 regardless of order.
        assert!((contrast_ratio([128, 128, 128], [128, 128, 128]) - 1.0).abs() < f64::EPSILON);
        // Primary ink on the light page surface is comfortably AA.
        let ink = parse_hex("#0b0b0b").unwrap();
        let page = parse_hex("#f9f9f7").unwrap();
        assert!(contrast_ratio(ink, page) > 15.0);
    }

    #[test]
    fn hex_parsing_supports_short_and_long_forms() {
        assert_eq!(parse_hex("#ffffff"), Some([255, 255, 255]));
        assert_eq!(parse_hex("#fff"), Some([255, 255, 255]));
        assert_eq!(parse_hex("#0b0b0b"), Some([11, 11, 11]));
        assert_eq!(parse_hex("f9f9f7"), None); // requires the leading #
        assert_eq!(parse_hex("#12345"), None);
    }

    #[test]
    fn shipped_token_foundation_passes_its_own_gate() {
        // The palette is validated BY CONSTRUCTION: if someone edits a token
        // value into an AA failure, this test names the exact pair.
        let findings = audit_token_contrast(&design_tokens_css());
        assert!(
            findings.is_empty(),
            "token foundation regressed: {findings:?}"
        );
    }

    #[test]
    fn token_audit_catches_a_broken_pair() {
        let broken = ":root {\n  --text-primary: #bbbbbb;\n  --surface-page: #cccccc;\n}\n";
        let findings = audit_token_contrast(broken);
        // The stylesheet defines no dark override, so the same failing pair
        // is reported for both themes (dark inherits the light values).
        assert_eq!(findings.len(), 2);
        assert!(findings[0].contains("--text-primary"));
        assert!(findings[0].contains("light"));
        assert!(findings[1].contains("dark"));
    }

    #[test]
    fn dark_theme_falls_back_to_light_values() {
        // --text-muted is not redefined in a dark block here; the dark audit
        // must reuse the light value instead of skipping the pair.
        let css = ":root {\n  --text-muted: #0b0b0b;\n  --surface-page: #ffffff;\n}\n\
                   [data-theme=\"dark\"] {\n  --surface-page: #000000;\n}\n";
        let (light, dark) = parse_theme_vars(css);
        assert_eq!(light.get("--text-muted"), Some(&[11, 11, 11]));
        assert_eq!(dark.get("--text-muted"), Some(&[11, 11, 11]));
        assert_eq!(dark.get("--surface-page"), Some(&[0, 0, 0]));
    }

    #[test]
    fn html_audit_flags_the_classic_failures() {
        let bad = "<html><head></head><body><img src=x><h1>a</h1><h1>b</h1>\
                   <a href=\"#\">click</a><p>lorem ipsum dolor</p></body></html>";
        let findings = audit_rendered_html(bad);
        let joined = findings.join("\n");
        assert!(joined.contains("lang"), "{joined}");
        assert!(joined.contains("viewport"), "{joined}");
        assert!(joined.contains("alt"), "{joined}");
        assert!(joined.contains("2 <h1>"), "{joined}");
        assert!(joined.contains("<main>"), "{joined}");
        assert!(joined.contains("dead link"), "{joined}");
        assert!(joined.contains("lorem ipsum"), "{joined}");
        assert!(joined.contains("<title>"), "{joined}");

        let good = "<html lang=\"es\"><head><title>Ok</title>\
                    <meta name=\"viewport\" content=\"width=device-width\"></head>\
                    <body><main><h1>Hola</h1><img src=x alt=\"logo\"></main></body></html>";
        assert!(audit_rendered_html(good).is_empty());
    }

    #[test]
    fn archetype_detection_prioritizes_specific_products() {
        let mut plan = Plan {
            vision: "una tienda online con carrito y checkout".to_string(),
            ..Plan::default()
        };
        assert_eq!(detect_archetype(&plan), DesignArchetype::Ecommerce);

        plan.vision = "dashboard de métricas para el equipo".to_string();
        assert_eq!(detect_archetype(&plan), DesignArchetype::Dashboard);

        plan.vision = "landing page para promocionar el producto".to_string();
        assert_eq!(detect_archetype(&plan), DesignArchetype::Landing);

        plan.vision = "un blog con documentación técnica".to_string();
        assert_eq!(detect_archetype(&plan), DesignArchetype::Content);

        plan.vision = "algo completamente distinto".to_string();
        assert_eq!(detect_archetype(&plan), DesignArchetype::General);
    }

    #[test]
    fn prompts_carry_the_contract_keywords() {
        let build = design_system_prompt(DesignArchetype::Saas, "src/styles/design-tokens.css");
        for needle in [
            "focus-visible",
            "aria-invalid",
            "design-system.md",
            "44px",
            "data-theme",
            "Skeleton",
            "EmptyState",
            "tabular-nums",
        ] {
            assert!(build.contains(needle), "missing {needle}");
        }
        let qa = visual_qa_prompt();
        for needle in ["h1", "320px", "lorem", "dark", "focus-visible", "Toast"] {
            assert!(qa.contains(needle), "missing {needle}");
        }
        let plan = Plan {
            vision: "tienda".to_string(),
            ..Plan::default()
        };
        let brief = designer_planning_brief(&plan);
        assert!(brief.contains("ecommerce"));
        assert!(brief.contains("WCAG AA"));
    }

    #[test]
    fn tokens_are_written_once_and_respected_after() {
        let dir = std::env::temp_dir().join(format!(
            "design-tokens-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let written = write_design_tokens(&dir).expect("first write");
        assert_eq!(written, "src/styles/design-tokens.css");
        assert!(dir.join(&written).exists());
        // Second call: a token stylesheet exists → do not overwrite.
        assert!(write_design_tokens(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
