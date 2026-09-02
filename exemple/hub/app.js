/**
 * perf-sentinel Hub launcher — shell, state and rendering.
 *
 * Classic script, no build step. Pure logic lives in launcher.js as `PSL`.
 *
 * Nothing from the server is ever written with innerHTML: every displayed
 * string is a text node, and nothing in this design needs rich HTML from data.
 */
(function () {
    "use strict";

    const PSL = globalThis.PSL;
    const THEME_KEY = "perf-sentinel:theme";
    const THEME_POSITIONS = ["auto", "light", "dark"];
    const THEME_LABELS = {auto: "System", light: "Light", dark: "Dark"};

    /** Glyph paths lifted from the dashboard's themeIcon(), not redrawn. */
    const THEME_GLYPHS = {
        auto: [["rect", {x: "3", y: "4", width: "18", height: "13", rx: "2"}], ["path", {d: "M8 21h8M12 17v4"}]],
        light: [["circle", {cx: "12", cy: "12", r: "4"}], ["path", {
            d: "M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"
        }]],
        dark: [["path", {d: "M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"}]]
    };

    const state = {
        themePosition: document.documentElement.getAttribute("data-theme-position") || "auto",
        screen: "new",
        status: null,
        sources: null,
        sourcesError: false,
        loading: true,
        run: null,
        runError: false,
        runTimer: null,
        noteTimer: null,
        // Every node whose text is a duration measured against the current moment,
        // and the single ticker that rewrites them. A countdown drawn once is wrong
        // a second later, and re-rendering the screen to refresh it would replace
        // the report iframe and reload the report underneath the reader.
        liveDurations: [],
        durationTicker: null,
        runs: null,
        // Which daemon rows are folded open, and what each one answered. Kept
        // across a render so a rebuilt table comes back the way it was left.
        daemonOpen: {},
        daemonViews: {},
        daemonSettingsOpen: {},
        daemonGroupOpen: {},
        // Poll interval per source in ms, 0 for off, and the one-second ticker
        // that drives both the countdown and the fetch.
        daemonRefreshMs: {},
        daemonTickers: {},
        // The read's own deadline, separate from the one-second ticker that writes
        // the countdown: a read timed off that ticker fires on the next whole
        // second, which is up to a second after the disc has already closed.
        daemonCycleTimers: {},
        daemonCycleAt: {},
        daemonInFlight: {},
        // When THIS browser last adopted a reading, on its own clock: subtracting
        // the Hub's observed_at_ms from Date.now() would bake the clock skew
        // between the two machines into "Read X ago".
        daemonReadAt: {},
        // When the last FULL read happened: light ticks carry the gauges alone,
        // and once a minute the full read re-syncs hints, state and settings age.
        daemonFullReadAt: {},
        daemonTerminalOpen: {},
        // What each gauge moved by on the read just adopted, shown once beside the
        // figure and then gone. Consumed by the render, so a rebuild for any other
        // reason does not replay a move that already had its moment.
        daemonMoves: {},
        // Folds that belong to a screen rather than to a source. Kept in the same
        // record, since a reader does not care which of the two a fold is.
        panelOpen: {},
        // Which shell every printed command is spelled for.
        shell: "posix",
        terminalSig: null,
        form: {
            sourceId: null,
            mode: "service",
            service: "",
            traceId: "",
            rangeMode: "relative",
            lookback: "1h",
            fromMs: Date.now() - 3600000,
            toMs: Date.now(),
            customQty: 90,
            customUnit: "m",
            detection: {},
            pickerOpen: false,
            maxTraces: 100,
            ackUnreachable: false,
            ackHeavy: false
        }
    };

    // ------------------------------------------------------------ DOM helpers

    function el(tag, attrs, children) {
        const node = document.createElement(tag);
        Object.keys(attrs || {}).forEach(function (key) {
            if (key === "class") node.className = attrs[key];
            else if (key === "text") node.textContent = attrs[key];
            else if (attrs[key] != null) node.setAttribute(key, String(attrs[key]));
        });
        (children || []).forEach(function (child) {
            if (child) node.appendChild(child);
        });
        return node;
    }

    function svg(paths, size) {
        const node = document.createElementNS("http://www.w3.org/2000/svg", "svg");
        node.setAttribute("viewBox", "0 0 24 24");
        node.setAttribute("fill", "none");
        node.setAttribute("stroke", "currentColor");
        node.setAttribute("stroke-width", "1.9");
        node.setAttribute("stroke-linecap", "round");
        node.setAttribute("stroke-linejoin", "round");
        node.setAttribute("aria-hidden", "true");
        if (size) {
            node.setAttribute("width", String(size));
            node.setAttribute("height", String(size));
        }
        paths.forEach(function (spec) {
            const shape = document.createElementNS("http://www.w3.org/2000/svg", spec[0]);
            Object.keys(spec[1]).forEach(function (key) {
                shape.setAttribute(key, spec[1][key]);
            });
            node.appendChild(shape);
        });
        return node;
    }

    // Write-only: the read happens in the inline script, before this file loads.
    // sessionStorage throws in Safari private mode and under some enterprise
    // policies, and a theme is not worth an error.
    function store(area, key, value) {
        try {
            globalThis[area].setItem(key, value);
        } catch (error) {
            // Nothing to do: the position still applies to this page.
        }
    }

    // ---------------------------------------------------------------- theme

    function resolveTheme(position) {
        if (position !== "auto") return position;
        return matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
    }

    function applyTheme(animate) {
        const root = document.documentElement;
        root.setAttribute("data-theme", resolveTheme(state.themePosition));
        root.setAttribute("data-theme-position", state.themePosition);
        // Both stores: localStorage so the position survives the tab, sessionStorage
        // because the rendered dashboard reads that exact key from this origin.
        store("localStorage", THEME_KEY, state.themePosition);
        store("sessionStorage", THEME_KEY, state.themePosition);

        const button = document.getElementById("theme-toggle");
        const glyph = document.getElementById("theme-glyph");
        document.getElementById("theme-label").textContent = THEME_LABELS[state.themePosition];
        button.setAttribute("aria-label", "Theme: " + THEME_LABELS[state.themePosition] + ". Click to cycle.");
        glyph.replaceChildren(svg(THEME_GLYPHS[state.themePosition], 15));
        if (!animate) return;
        // Two identical keyframes alternated, to force the animation to restart.
        button.setAttribute("data-spin", button.getAttribute("data-spin") === "a" ? "b" : "a");
    }

    function initTheme() {
        document.getElementById("theme-toggle").addEventListener("click", function () {
            const next = (THEME_POSITIONS.indexOf(state.themePosition) + 1) % THEME_POSITIONS.length;
            state.themePosition = THEME_POSITIONS[next];
            applyTheme(true);
        });
        // An OS change re-resolves live and never animates, or the theme would
        // spin by itself at sunset.
        matchMedia("(prefers-color-scheme: light)").addEventListener("change", function () {
            if (state.themePosition === "auto") applyTheme(false);
        });
        applyTheme(false);
    }

    // ----------------------------------------------------------------- data

    function getJson(path) {
        return fetch(path, {headers: {accept: "application/json"}}).then(function (response) {
            if (!response.ok) throw new Error(path + " answered " + response.status);
            return response.json();
        });
    }

    function loadShell() {
        return Promise.all([
            getJson("/api/status").catch(function () {
                return null;
            }),
            getJson("/api/sources").catch(function () {
                return "error";
            })
        ]).then(function (results) {
            state.status = results[0];
            state.sourcesError = results[1] === "error";
            state.sources = state.sourcesError ? null : results[1];
            state.loading = false;
            if (state.sources && state.form.sourceId === null) {
                // The one they were on last, if it is still configured. Falling back to
                // the first reachable one is what a first visit gets.
                const remembered = rememberedSource();
                const kept = state.sources.find(function (source) {
                    return source.id === remembered;
                });
                const usable = state.sources.find(function (source) {
                    return source.reachable;
                });
                state.form.sourceId = (kept || usable || state.sources[0] || {}).id || null;
            }
            renderShell();
            onRoute();
        });
    }

    // Folds outlive the page. A reader who opened a row, its settings and two of
    // its groups should find all four the way they left them, so the four maps
    // are one record rather than four, written whenever one of them changes.
    const FOLD_STORAGE_KEY = "perf-sentinel-hub.folds";
    // Its own key rather than a field in the fold record: a chosen source is not
    // a fold, and one name per thing survives the next thing worth remembering.
    const SOURCE_STORAGE_KEY = "perf-sentinel-hub.source";
    const SHELL_STORAGE_KEY = "perf-sentinel-hub.shell";

    /**
     * The shell every printed command is spelled for. The reader's own choice
     * once they make one, and until then the one their platform opens with.
     */
    function restoreShell() {
        let stored = null;
        try {
            stored = localStorage.getItem(SHELL_STORAGE_KEY);
        } catch (error) {
            stored = null;
        }
        const platform = (navigator.userAgentData && navigator.userAgentData.platform)
            || navigator.platform;
        // A stored id that no longer names a shell is worth no more than no id at
        // all, so it falls back to the platform rather than to the first entry.
        state.shell = PSL.knownShell(stored) || PSL.defaultShell(platform);
    }

    function saveShell(id) {
        try {
            localStorage.setItem(SHELL_STORAGE_KEY, id);
        } catch (error) {
            // Storage refused. The choice holds for this page and the next visit
            // opens on the platform's own shell again.
        }
    }

    function rememberedSource() {
        try {
            return localStorage.getItem(SOURCE_STORAGE_KEY);
        } catch (error) {
            return null;
        }
    }

    function saveSource(id) {
        try {
            if (id) localStorage.setItem(SOURCE_STORAGE_KEY, id);
        } catch (error) {
            // Storage refused. The next visit opens on the default source, which is
            // where every visit used to open.
        }
    }

    function restoreFolds() {
        let stored = null;
        try {
            stored = JSON.parse(localStorage.getItem(FOLD_STORAGE_KEY) || "null");
        } catch (error) {
            // Storage refused, or held something that is not JSON. Everything starts
            // folded, which is what a first visit gets anyway.
            stored = null;
        }
        if (!stored || typeof stored !== "object") return;
        state.daemonOpen = PSL.openFolds(stored.row);
        state.daemonSettingsOpen = PSL.openFolds(stored.settings);
        state.daemonGroupOpen = PSL.openFolds(stored.group);
        state.daemonTerminalOpen = PSL.openFolds(stored.terminal);
        state.panelOpen = PSL.openFolds(stored.panel);
    }

    function saveFolds() {
        try {
            localStorage.setItem(FOLD_STORAGE_KEY, JSON.stringify({
                row: PSL.openFolds(state.daemonOpen),
                settings: PSL.openFolds(state.daemonSettingsOpen),
                group: PSL.openFolds(state.daemonGroupOpen),
                terminal: PSL.openFolds(state.daemonTerminalOpen),
                panel: PSL.openFolds(state.panelOpen)
            }));
        } catch (error) {
            // A full or disabled store costs the reader the memory of their folds
            // and nothing else, so there is nothing to report here.
        }
    }

    function renderShell() {
        const status = state.status;
        document.getElementById("version-hub").textContent = status ? status.version : "unknown";
        document.getElementById("version-engine").textContent =
            status && status.engine_version ? status.engine_version : "none";
        if (status) PSL.setVersions(status.version, status.engine_version);
        renderUpdates();

        // The identity comes from the reverse proxy. With no proxy in front there
        // is nothing to show, and an empty chip is better than a fake name.
        const identity = document.getElementById("identity");
        identity.hidden = !status || !status.identity;
        document.getElementById("identity-name").textContent = status && status.identity ? status.identity : "";

        renderFleetSkew();
        renderSourcesBadge();
    }

    /** Takes a chip segment out along with the separator that was added for it. */
    function dropSegment(chip, selector) {
        const existing = chip.querySelector(selector);
        if (!existing) return;
        const rule = existing.previousElementSibling;
        if (rule && rule.classList.contains("shell-version-rule")) rule.remove();
        existing.remove();
    }

    /**
     * What the Hub last heard from GitHub, and only when it is news. Absent says
     * nothing either way: the check may be off, may not have run, or may not have
     * reached anything, and none of those mean the versions here are current.
     */
    function renderUpdates() {
        const chip = document.getElementById("version-chip");
        // The rule belongs to the segment, so it goes with it. Removing one and not
        // the other leaves a separator behind on every rebuild.
        dropSegment(chip, ".shell-version-update");
        if (!state.status) return;

        const behind = [
            ["hub", PSL.updateState(state.status.version, state.status.latest_hub_version), PSL.hubReleaseUrl()],
            ["engine", PSL.updateState(state.status.engine_version, state.status.latest_engine_version), null]
        ].filter(function (row) {
            return row[1];
        });
        if (behind.length === 0) return;

        const segment = el("span", {
            class: "shell-version-update",
            title: "The newest release published on GitHub, read by this Hub on its update-check "
                + "interval. It says a newer version exists, not that this one is wrong."
        }, [
            svg([["path", {d: "M12 19V5M5 12l7-7 7 7"}]], 12),
            // Said, not left to be inferred from two numbers side by side. The Hub
            // asked and got an answer, so it can claim this much.
            el("span", {class: "shell-version-news", text: "update available"})
        ]);
        behind.forEach(function (row, index) {
            segment.appendChild(el("span", {text: index === 0 ? ":" : "\u00b7"}));
            segment.appendChild(el("a", {
                href: row[2] || PSL.releaseUrl(row[1].latest),
                target: "_blank",
                rel: "noopener noreferrer",
                text: row[0] + " " + row[1].latest
            }));
        });
        chip.appendChild(el("span", {class: "shell-version-rule", "aria-hidden": "true"}));
        chip.appendChild(segment);
    }

    /**
     * The last version segment, and it appears only when a producer is behind the
     * engine. It names the spread across the fleet, not one source.
     */
    function renderFleetSkew() {
        const chip = document.getElementById("version-chip");
        dropSegment(chip, ".shell-version-skew");
        if (!state.sources || !state.status || !state.status.engine_version) return;

        // Behind only. A fleet ahead of the Hub is a normal moment during a rollout,
        // and the arrow would have pointed from the newer version down to the older
        // one, which reads as an instruction to downgrade.
        const behind = state.sources
            .map(function (source) {
                return source.producer_version;
            })
            .filter(function (version) {
                const skew = version && PSL.skew(version);
                return skew && skew.dir === "behind";
            });
        if (behind.length === 0) return;

        const oldest = behind.sort(PSL.vcmp)[0];
        chip.appendChild(el("span", {class: "shell-version-rule", "aria-hidden": "true"}));
        chip.appendChild(el("span", {class: "shell-version-skew"}, [
            svg([["path", {d: "M12 4l9 16H3z"}], ["path", {d: "M12 10v4M12 17.4v.2"}]], 12),
            el("span", {text: "fleet " + oldest + " → " + state.status.engine_version})
        ]));
    }

    function renderSourcesBadge() {
        const badge = document.getElementById("sources-badge");
        const unreachable = (state.sources || []).filter(function (source) {
            return !source.reachable;
        });
        badge.hidden = unreachable.length === 0;
        badge.textContent = String(unreachable.length);
    }

    // ------------------------------------------------------------ navigation

    function currentScreen() {
        const hash = (location.hash || "#/new").replace("#/", "");
        if (hash.indexOf("run/") === 0) return "run";
        if (hash.indexOf("report/") === 0) return "report";
        return ["new", "recent", "sources"].indexOf(hash) >= 0 ? hash : "new";
    }

    function currentRunId() {
        const match = /^#\/(?:run|report)\/([0-9a-f]{16})$/.exec(location.hash || "");
        return match ? match[1] : null;
    }

    /**
     * Marks a node's text as a live duration. `compute` is re-run every second and
     * its result written back, so a caller passes the same function it used to
     * build the initial text.
     */
    function live(node, compute) {
        if (typeof compute === "function") state.liveDurations.push({node: node, compute: compute});
        return node;
    }

    function startDurationTicker() {
        if (!state.liveDurations.length) return;
        state.durationTicker = setInterval(function () {
            state.liveDurations.forEach(function (entry) {
                // A failed resubmission borrows this line for six seconds and stashes
                // the original on the node. Rewriting it now would wipe the error
                // before anyone read it.
                if (entry.node.dataset && entry.node.dataset.restore !== undefined) return;
                // Per entry: one compute that throws must not freeze every countdown
                // behind it in the list for the rest of the screen's life.
                try {
                    const text = entry.compute();
                    entry.node.textContent = text;
                    // Some cells carry the same string as a tooltip. Left alone it would
                    // still read the value this screen was built with.
                    if (entry.node.title) entry.node.title = text;
                } catch (error) {
                    void error;
                }
            });
        }, 1000);
    }

    function stopDurationTicker() {
        clearInterval(state.durationTicker);
        state.durationTicker = null;
        // The nodes themselves are about to be replaced, so the registry goes too.
        state.liveDurations = [];
    }

    function render() {
        // A tip whose anchor is about to be replaced would never see its mouseleave.
        closeTip();
        // Same for the daemon tickers: they write into nodes this render replaces.
        stopAllTickers();
        stopDurationTicker();
        state.screen = currentScreen();
        Array.prototype.forEach.call(document.querySelectorAll(".shell-tab"), function (tab) {
            if (tab.getAttribute("data-screen") === state.screen) tab.setAttribute("aria-current", "page");
            else tab.removeAttribute("aria-current");
        });

        const main = document.getElementById("main");
        document.body.setAttribute("data-screen", state.screen);
        if (state.loading && state.screen !== "report") {
            main.replaceChildren(el("div", {class: "card skeleton", style: "height:220px"}));
            return;
        }
        // Every screen reads limits, workers and the engine version off the status.
        // Without it there is nothing truthful to draw, and reaching for it would
        // throw on the first field.
        if (!state.status) {
            main.replaceChildren(hubUnreachableBanner());
            return;
        }
        if (state.screen === "sources") main.replaceChildren(renderSourcesScreen());
        else if (state.screen === "new") main.replaceChildren(renderNewScreen());
        else if (state.screen === "run") main.replaceChildren(renderRunScreen(currentRunId()));
        else if (state.screen === "report") main.replaceChildren(renderReportScreen(currentRunId()));
        else main.replaceChildren(renderRecentScreen());
        startDurationTicker();
    }

    /** The label with a rule running out to its right, as every screen head has. */
    function ruledOverline(text) {
        return el("div", {class: "overline-ruled"}, [
            el("span", {class: "overline", text: text}),
            el("span", {class: "overline-rule", "aria-hidden": "true"})
        ]);
    }

    // ------------------------------------------------- shared: help affordance

    /**
     * One floating tip at a time, on <body>. The Sources table scrolls
     * horizontally and clips its own rows, so a tip anchored in CSS cannot
     * escape it. The native title attribute is not an option either: it never
     * appears on keyboard focus.
     */
    let openTip = null;

    function closeTip() {
        if (!openTip) return;
        openTip.remove();
        openTip = null;
    }

    /**
     * The dashboard's help affordance, same shape and same rule: it sits beside
     * a heading and never inside one, or a screen reader would read the
     * explanation as part of the heading.
     */
    function helpDot(text) {
        const dot = el("button", {type: "button", class: "help-dot", "aria-label": text, text: "?"});
        dot.addEventListener("mouseenter", function () {
            showTip(dot, text);
        });
        dot.addEventListener("focus", function () {
            showTip(dot, text);
        });
        dot.addEventListener("mouseleave", closeTip);
        dot.addEventListener("blur", closeTip);
        // Touch has no hover, and the synthesized mouseenter and focus that come
        // before a tap would make a toggle close its own tip. Showing is
        // idempotent, and blur or leaving the dot is what closes.
        dot.addEventListener("click", function () {
            showTip(dot, text);
        });
        return dot;
    }

    function showTip(anchor, text) {
        closeTip();
        const box = el("div", {class: "tipbox", role: "tooltip", text: text});
        document.body.appendChild(box);
        const at = anchor.getBoundingClientRect();
        const size = box.getBoundingClientRect();
        let left = at.right + 10;
        let top = at.top + at.height / 2 - size.height / 2;
        if (left + size.width > globalThis.innerWidth - 12) {
            // No room beside it, so it sits underneath instead.
            left = Math.min(at.left - 16, globalThis.innerWidth - size.width - 12);
            top = at.bottom + 10;
        }
        box.style.left = Math.max(12, left) + "px";
        box.style.top = Math.max(12, top) + "px";
        openTip = box;
    }

    /**
     * Text with its backticked spans rendered as code.
     *
     * The daemon writes setting names in backticks, the way its own documentation
     * does. The terminal monitor strips them because a terminal cannot render
     * them, and a browser can. An odd number of backticks is left alone rather
     * than guessed at, so a stray one never turns the rest of a sentence into
     * code.
     */
    function proseInto(parent, text) {
        const parts = String(text).split("`");
        if (parts.length % 2 === 0) {
            parent.appendChild(document.createTextNode(String(text)));
            return parent;
        }
        parts.forEach(function (part, index) {
            if (part === "") return;
            if (index % 2 === 1) parent.appendChild(el("code", {class: "code-inline", text: part}));
            else parent.appendChild(document.createTextNode(part));
        });
        return parent;
    }

    /** A label with its "?" beside it, which is the only place one belongs. */
    function titledOverline(text, help) {
        return el("div", {class: "title-row"}, [
            el("span", {class: "overline", text: text}),
            help ? helpDot(help) : null
        ]);
    }

    // ------------------------------------------------ shared: a copyable command

    /**
     * A command, and one button that copies it.
     *
     * The text arrives already joined: which flags belong on which line is shell
     * syntax rather than a layout choice, so it is decided in PSL beside the
     * quoting.
     */
    /**
     * Every command this product prints runs the engine binary, not the Hub, so
     * it needs one on the machine it is typed into. The link pins the version
     * this Hub runs: that is the one the command above is spelled for.
     */
    function engineNeed(parent) {
        const version = PSL.ENGINE;
        proseInto(parent, version
            ? "Install the `perf-sentinel` binary on the machine you will type this into. This Hub "
            + "runs " + version + " and the command is spelled for it, an older engine may refuse a "
            + "flag it does not have yet. Take the same build: "
            : "Install the `perf-sentinel` binary on the machine you will type this into. This Hub "
            + "reports no engine version, so there is none to match here: ");
        parent.appendChild(el("a", {
            class: "terminal-link",
            href: PSL.releaseUrl(version),
            // Someone else's site in someone else's tab: the Hub keeps the page the
            // reader was working on, and hands the opener nothing.
            target: "_blank",
            rel: "noopener noreferrer",
            text: version ? "perf-sentinel " + version + " on GitHub" : "the perf-sentinel releases"
        }));
        parent.appendChild(document.createTextNode("."));
        return parent;
    }

    function engineNote() {
        return engineNeed(el("p", {class: "terminal-note"}));
    }

    /**
     * The step that trips people up: it names a variable, and a reader who has
     * never met the flag cannot tell what goes in it or where. So it shows the
     * line to run, in the shell they picked, with their own credential to drop
     * in. Only ever shown for a source the Hub itself reaches with a header, so
     * an open backend on an intranet never sees any of this.
     */
    function tokenStep(source) {
        const header = source.auth_header_name + ": …";
        const step = el("span", {});
        proseInto(step, "This source is behind an `" + source.auth_header_name + "` header. Run this "
            + "in the same terminal, before the command, with your own credential where the dots are:");
        const line = el("code", {
            class: "code-inline step-line",
            text: PSL.exportLine(state.shell, "PERF_SENTINEL_SOURCE_TOKEN", header)
        });
        SPELL.set(line, function (shellId) {
            return PSL.exportLine(shellId, "PERF_SENTINEL_SOURCE_TOKEN", header);
        });
        line.setAttribute("data-spell", "");
        step.appendChild(line);
        proseInto(step, "The whole header line goes in, name included, not the value on its own. The "
            + "Hub holds a header of its own for this source and never discloses it, which is why the "
            + "command reads yours from a variable rather than carrying a secret where `ps` would "
            + "show it to everyone on the machine.");
        return step;
    }

    /** An arrow turning back on itself: put this value where it was. */
    function undoGlyph(size) {
        return svg([
            ["polyline", {points: "3 5 3 11 9 11"}],
            ["path", {d: "M5.1 15.5a8 8 0 1 0 1.9-8.3L3 11"}]
        ], size);
    }

    /** A circled i, for a block that tells rather than warns. */
    function infoGlyph(size) {
        return svg([
            ["circle", {cx: "12", cy: "12", r: "9"}],
            ["path", {d: "M12 11.2v5M12 7.8v.2"}]
        ], size);
    }

    /**
     * What has to be true before the command above can run, in the order you
     * would do it. Prose that only explains stays prose below: a numbered step
     * is a promise that there is something to go and do.
     */
    function stepsBlock(steps) {
        const present = steps.filter(Boolean);
        return el("div", {class: "steps-block"}, [
            el("div", {class: "steps-head"}, [
                infoGlyph(14),
                el("span", {class: "overline", text: "// what you need first"})
            ]),
            // The content is always one node: the row is a two-column grid, and prose
            // carrying a code chip would otherwise arrive as several grid items and
            // be dealt into the columns one piece at a time.
            el("ol", {class: "steps"}, present.map(function (step) {
                return el("li", {}, [step instanceof Node
                    ? el("span", {class: "step-text"}, [step])
                    : proseInto(el("span", {class: "step-text"}), step)]);
            }))
        ]);
    }

    /**
     * One tab per shell, over a single command line. The quoting and the line
     * continuation differ between them, so this is the same request written
     * three ways rather than a preference about how it looks.
     */
    function shellTabs(code, spell) {
        const tabs = PSL.SHELLS.map(function (shell) {
            const tab = el("button", {
                type: "button",
                class: "cmd-tab",
                role: "tab",
                "data-shell": shell.id,
                "aria-selected": shell.id === state.shell ? "true" : "false",
                tabindex: shell.id === state.shell ? "0" : "-1",
                text: shell.label
            });
            tab.addEventListener("click", function () {
                choose(shell.id);
            });
            return tab;
        });

        function choose(id) {
            state.shell = id;
            saveShell(id);
            // Every printed command on the page follows: the reader chose a shell,
            // not a tab on one block.
            document.querySelectorAll("[data-spell]").forEach(function (node) {
                const spellFor = SPELL.get(node);
                if (spellFor) node.textContent = spellFor(id);
            });
            document.querySelectorAll(".cmd-tab").forEach(function (node) {
                // By id, not by the label a reader sees: two shells could share a word.
                const selected = node.getAttribute("data-shell") === id;
                node.setAttribute("aria-selected", selected ? "true" : "false");
                node.setAttribute("tabindex", selected ? "0" : "-1");
            });
        }

        const strip = el("div", {class: "cmd-tabs", role: "tablist", "aria-label": "Shell"}, tabs);
        // Arrow keys move between tabs, which is what a tablist promises.
        strip.addEventListener("keydown", function (event) {
            const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
            if (!step) return;
            event.preventDefault();
            const index = PSL.SHELLS.findIndex(function (shell) {
                return shell.id === state.shell;
            });
            const next = PSL.SHELLS[(index + step + PSL.SHELLS.length) % PSL.SHELLS.length];
            choose(next.id);
            tabs[PSL.SHELLS.indexOf(next)].focus();
        });
        SPELL.set(code, spell);
        code.setAttribute("data-spell", "");
        return strip;
    }

    // The line each command block would print for a given shell, kept beside the
    // node rather than rebuilt from the form, which has moved on by then.
    const SPELL = new WeakMap();

    function terminalBlock(spec) {
        const spell = spec.spell || null;
        const code = el("pre", {
            class: "terminal-code",
            tabindex: "0",
            text: spell ? spell(state.shell) : spec.text
        });
        if (spec.id) code.id = spec.id;
        const status = el("span", {class: "terminal-status", role: "status"});
        // What this block holds, for the sentences below: the same scaffold renders
        // a command and a configuration file, and neither noun fits both.
        const subject = spec.download ? "file" : "command";
        const label = el("span", {text: "Copy"});
        const button = el("button", {
            type: "button",
            class: "pill-button terminal-copy",
            "aria-label": spec.copyLabel
        }, [copyGlyph(), label]);

        // One timer per block, held here rather than on state: two blocks are on
        // screen at once, and a shared handle would let one revert cancel the other.
        let timer = 0;
        button.addEventListener("click", function () {
            // Read off the node, not off the spec: a command that changes under the
            // reader would otherwise copy the line it replaced.
            writeClipboard(code).then(function (copied) {
                label.textContent = copied ? "Copied" : "Copy";
                if (copied) button.setAttribute("data-copied", "true");
                status.textContent = copied
                    ? "Copied."
                    : "This browser refused the copy. The " + subject
                    + " is selected, use your own copy key.";
                clearTimeout(timer);
                timer = setTimeout(function () {
                    label.textContent = "Copy";
                    button.removeAttribute("data-copied");
                    status.textContent = "";
                }, 3200);
            });
        });

        const body = el("div", {class: "terminal-body"}, [
            spell ? shellTabs(code, spell) : null,
            code,
            el("div", {class: "terminal-actions"},
                spec.download ? [button, downloadButton(code, spec.download, status), status] : [button, status])
        ]);
        (spec.notes || []).forEach(function (note) {
            if (!note) return;
            body.appendChild(note instanceof Node
                ? note
                : proseInto(el("p", {class: "terminal-note"}), note));
        });
        if (!spec.fold) {
            return el("section", {class: "card terminal"}, [
                el("div", {class: "terminal-head"}, [
                    titledOverline(spec.head, spec.help),
                    el("span", {class: "terminal-sub", text: spec.sub})
                ]),
                body
            ]);
        }

        // Folded, the overline and its subtitle still say what is behind it: a
        // lone chevron would make the reader open it to find out.
        const toggle = el("button", {
            type: "button",
            class: "terminal-more",
            "aria-expanded": spec.fold.open ? "true" : "false"
        }, [el("span", {class: "overline", text: spec.head})]);
        body.hidden = !spec.fold.open;
        toggle.addEventListener("click", function () {
            const next = toggle.getAttribute("aria-expanded") !== "true";
            toggle.setAttribute("aria-expanded", next ? "true" : "false");
            body.hidden = !next;
            spec.fold.onToggle(next);
        });
        return el("section", {class: "card terminal"}, [
            el("div", {class: "terminal-head"}, [
                toggle,
                spec.help ? helpDot(spec.help) : null,
                el("span", {class: "terminal-sub", text: spec.sub})
            ]),
            body
        ]);
    }

    /**
     * Hands the block's text over as a file, for a panel whose content is meant
     * to become one. Copying leaves the reader to create the file themselves,
     * which is a step the Hub can take for them.
     *
     * The blob is built at click time from the node, like the copy button, so a
     * fragment that changed under the reader is not the one that lands on disk.
     */
    function downloadButton(code, filename, status) {
        const button = el("button", {
            type: "button",
            class: "pill-button terminal-download",
            "aria-label": "Download " + filename
        }, [downloadGlyph(), el("span", {text: "Download"})]);
        let timer = 0;
        button.addEventListener("click", function () {
            const url = URL.createObjectURL(new Blob([code.textContent], {type: "application/toml"}));
            // No insertion: a programmatic click on an anchor carrying a download
            // attribute does not need the element to be in the document.
            const link = el("a", {href: url, download: filename});
            link.click();
            // Long enough for the browser to have taken the blob, short enough not to
            // hold one per click for half a minute.
            setTimeout(function () {
                URL.revokeObjectURL(url);
            }, 4000);
            // Said, because a blocked download is otherwise indistinguishable from a
            // click that was never received.
            status.textContent = "Sent " + filename + " to your browser. It decides where it lands, "
                + "and may rename a file whose name starts with a dot.";
            clearTimeout(timer);
            timer = setTimeout(function () {
                status.textContent = "";
            }, 6000);
        });
        return button;
    }

    function downloadGlyph() {
        return svg([
            ["path", {d: "M12 4v10M8 11l4 4 4-4"}],
            ["path", {d: "M5 19h14"}]
        ], 14);
    }

    /**
     * Copies, or selects and says so.
     *
     * navigator.clipboard exists only in a secure context and this Hub is
     * routinely served over plain HTTP on an internal address. The check comes
     * before the call rather than after a rejection: Firefox exposes the object
     * in an insecure context and rejects, and by the time that promise settles
     * the user activation execCommand needs is gone.
     *
     * When even that is refused, the selection it made is left in place: the
     * operator finishes with their own keystroke, and the block says so instead
     * of pretending the click worked.
     */
    function writeClipboard(code) {
        const text = code.textContent;
        if (globalThis.isSecureContext && navigator.clipboard && navigator.clipboard.writeText) {
            // The label waits for this promise: claiming "Copied" before it settles
            // would announce a success a denied permission can still take back.
            return navigator.clipboard.writeText(text).then(
                function () {
                    return true;
                },
                function () {
                    selectNode(code);
                    return false;
                });
        }
        const selection = selectNode(code);
        let copied = false;
        try {
            copied = document.execCommand("copy");
        } catch (error) {
            copied = false;
        }
        if (copied && selection) selection.removeAllRanges();
        return Promise.resolve(copied);
    }

    function selectNode(node) {
        const range = document.createRange();
        range.selectNodeContents(node);
        const selection = getSelection();
        if (!selection) return null;
        selection.removeAllRanges();
        selection.addRange(range);
        return selection;
    }

    function copyGlyph() {
        return svg([
            ["rect", {x: "9", y: "9", width: "11", height: "11", rx: "2"}],
            ["path", {d: "M5 15V5a2 2 0 0 1 2-2h10"}]
        ], 13);
    }

    /** Loads whatever the route needs, then renders it. */
    function onRoute() {
        const screen = currentScreen();
        clearTimeout(state.runTimer);
        // The note this timer restores belongs to a panel the next render replaces.
        clearTimeout(state.noteTimer);
        render();
        if (state.loading) return;
        // Both screens read the runs list: recent renders it, the launcher
        // shows what past runs weighed. Reloaded on every entry, not once, so
        // coming back from a run that just finished shows its weight.
        if (screen === "recent" || screen === "new") loadRuns();
        else if (screen === "run" || screen === "report") {
            const id = currentRunId();
            if (id && (!state.run || state.run.id !== id)) loadRun(id);
            else if (id) render();
        }
    }

    // -------------------------------------------------------- screen: sources

    function renderSourcesScreen() {
        const section = el("section", {}, [
            ruledOverline("// sources"),
            el("h1", {class: "page-title", text: "Fleet health"}),
            el("p", {
                class: "page-sub",
                text: "Everything on this screen is an observation except the environment column, which is "
                    + "declared. Sources are configured at deploy time, and the launcher cannot add one."
            })
        ]);

        if (state.loading) {
            section.appendChild(el("div", {class: "sources-wrap"}, [skeletonTable()]));
            return section;
        }
        if (state.sourcesError) {
            // Showing the last known values here would be worse than showing none:
            // a stale health table is the one thing this page must never be.
            section.appendChild(el("div", {class: "banner", "data-tone": "crit"}, [
                critGlyph(16),
                el("div", {
                    text: "The Hub is not answering, so fleet health is unknown. This is the Hub itself, "
                        + "not any one source. Nothing below is shown rather than showing values that may be stale."
                })
            ]));
            return section;
        }

        section.appendChild(el("div", {class: "sources-wrap"}, [sourcesTable(state.sources)]));
        section.appendChild(el("p", {
            class: "sources-note",
            text: "The environment column is declared by each source's own configuration and is never "
                + "measured. A misconfigured deployment can label production as staging."
        }));
        return section;
    }

    const SOURCE_COLUMNS = [
        "Source", "Type", "Env (declared)", "Health", "Last success", "Unreachable for", "Producer", "Last error"
    ];
    // The columns whose cells are right-aligned. The headings have to follow, or
    // a value sits under the gap beside its own label.
    const SOURCE_COLUMNS_RIGHT = ["Last success", "Unreachable for", "Producer"];

    function sourcesTable(sources) {
        const head = el("tr", {}, SOURCE_COLUMNS.map(function (name) {
            return el("th", {
                text: name,
                scope: "col",
                "data-align": SOURCE_COLUMNS_RIGHT.indexOf(name) >= 0 ? "right" : null
            });
        }));
        // PSL.splitByKind keeps each source's original position, which the fold ids
        // are built from, so a grouped row still addresses the source it belongs to.
        const kinds = PSL.splitByKind(sources);
        const rows = function (group) {
            return group.flatMap(function (entry) {
                return sourceRow(entry.source, entry.index);
            });
        };
        const body = kinds.split
            ? [tableGroupRow("daemons")].concat(rows(kinds.daemons),
                [tableGroupRow("trace backends")],
                rows(kinds.backends))
            : sources.flatMap(sourceRow);
        return el("table", {class: "table"}, [
            el("thead", {}, [head]),
            el("tbody", {}, body)
        ]);
    }

    function tableGroupRow(text) {
        return el("tr", {class: "table-group"}, [
            el("td", {colspan: String(SOURCE_COLUMNS.length)}, [el("span", {class: "overline", text: text})])
        ]);
    }

    function sourceRow(source, index) {
        const now = Date.now();
        const row = el("tr", source.reachable ? {} : {"data-unreachable": "true"});
        // Only a daemon has something to unfold, so only a daemon gets a control.
        if (source.kind === "daemon") row.appendChild(daemonNameCell(source, index));
        else row.appendChild(el("td", {class: "table-strong", text: source.name}));
        row.appendChild(el("td", {}, [el("span", {class: "chip", text: PSL.KIND_LABEL[source.kind] || source.kind})]));
        row.appendChild(el("td", {}, [el("span", {class: "chip chip-declared", text: source.environment})]));
        row.appendChild(el("td", {}, [healthCell(source, now)]));
        row.appendChild(el("td", {
            "data-align": "right",
            text: source.last_success_ms ? PSL.dur(now - source.last_success_ms) + " ago" : "never"
        }));
        row.appendChild(el("td", {
            "data-align": "right",
            text: source.unreachable_since_ms ? PSL.dur(now - source.unreachable_since_ms) : "—"
        }));
        row.appendChild(producerCell(source));
        row.appendChild(el("td", {class: "table-mono", text: source.last_error_code || "—"}));
        if (source.kind !== "daemon") return [row];

        // Spanning the header count and not the literal 8: adding a column must
        // not be able to break the detail row.
        const cell = el("td", {
            id: "daemon-detail-" + index,
            colspan: String(SOURCE_COLUMNS.length)
        }, [daemonPanel(source, index)]);
        const detail = el("tr", {class: "daemon-detail"}, [cell]);
        detail.hidden = state.daemonOpen[source.id] !== true;
        // A render stopped every ticker, so a row rebuilt open re-arms its own
        // once the table is attached, and one opened by a link from another
        // screen runs its first read here.
        if (state.daemonOpen[source.id] === true) {
            queueMicrotask(function () {
                const view = state.daemonViews[source.id];
                if (view === undefined) loadDaemon(source, index);
                else if (view !== "loading" && !view.error_code) startTicker(source, index);
            });
        }
        return [row, detail];
    }

    function healthCell(source, now) {
        if (source.reachable) {
            return el("span", {class: "health", "data-health": "ok"}, [
                el("span", {class: "health-dot"}),
                el("span", {text: source.last_attempt_ms == null ? "not yet observed" : "reachable"})
            ]);
        }
        return el("span", {class: "health", "data-health": "crit"}, [
            el("span", {class: "health-dot"}),
            el("span", {text: "unreachable " + PSL.dur(now - source.unreachable_since_ms)})
        ]);
    }

    function producerCell(source) {
        if (!source.producer_version) {
            // Two different absences. A backend has no producer at all, and saying
            // so about a daemon nobody has reached yet would be a false statement
            // about a source that does have one.
            return source.kind === "daemon"
                ? el("td", {
                    class: "table-muted",
                    "data-align": "right",
                    text: "unknown",
                    title: "This daemon reports a producer version, but the Hub has not had a successful "
                        + "response from it yet."
                })
                : el("td", {
                    class: "table-muted",
                    "data-align": "right",
                    text: "n/a",
                    title: "A trace backend stores traces and detects nothing, so it reports no producer version."
                });
        }

        const cell = el("td", {
            class: "table-mono",
            "data-align": "right"
        }, [el("span", {text: source.producer_version})]);
        const gap = PSL.skew(source.producer_version);
        if (gap) {
            cell.appendChild(el("span", {
                class: "skew-pill",
                "data-dir": gap.dir,
                text: gap.label,
                title: "perf-sentinel is pre-1.0, so detectors change between minors. The Hub compares two "
                    + "version strings and cannot know whether this minor changed detection."
            }));
        }
        return cell;
    }

    function skeletonTable() {
        const rows = [];
        for (let index = 0; index < 4; index++) rows.push(el("div", {class: "skeleton skeleton-row"}));
        return el("div", {class: "skeleton-stack"}, rows);
    }


    // ------------------------------------------------- sources: the daemon row

    /**
     * The [daemon] section in the order the daemon's own monitor prints it, so
     * the two surfaces never contradict each other. Environment stands alone:
     * it is the one value that claims something about the world rather than
     * about the process.
     */
    const DAEMON_GROUPS = [
        ["// declared", "Not measured. This is what the daemon says it is.", ["environment"]],
        ["// ingestion and memory", "What it takes in, and what it drops when it cannot keep up.",
            ["sampling_rate", "max_active_traces", "trace_ttl_ms", "max_events_per_trace",
                "max_payload_size", "ingest_queue_capacity", "analysis_queue_capacity",
                "memory_high_water_pct"]],
        ["// what it keeps for readers",
            "Ring buffers behind the query API and the export. Past each one, the oldest goes.",
            ["max_retained_findings", "max_export_findings", "max_retained_traces"]],
        ["// listeners", "Where it accepts spans, and whether it answers questions at all.",
            ["api_enabled", "listen_addr", "listen_port", "listen_port_grpc", "json_socket"]],
        ["// sub-systems", "Off unless somebody turned them on.",
            ["tls_configured", "ack_enabled", "ack_api_key_set", "cors_allowed_origins",
                "archive_configured"]],
        ["// correlation",
            "Off by default. Every field under the first one applies only while it is on.",
            ["correlation_enabled", "correlation_window_ms", "correlation_lag_threshold_ms",
                "correlation_min_co_occurrences", "correlation_min_confidence",
                "correlation_max_tracked_pairs"]]
    ];

    /** What each setting costs when it is wrong. Two say enough by their name. */
    const DAEMON_COPY = {
        environment: "The label this daemon stamps on every finding, which sets their confidence: "
            + "staging reads as medium, production as high. Declared, like the column on the row above.",
        sampling_rate: "The share of arriving traces it analyses. Below 100 % every aggregate in a "
            + "snapshot is a sample of the traffic, not the traffic.",
        max_active_traces: "How many traces it correlates in memory at once. The oldest is evicted "
            + "past this, and a trace evicted while spans are still arriving is analysed incomplete.",
        trace_ttl_ms: "How long a trace waits for more spans before it is closed and analysed.",
        max_events_per_trace: "The ring buffer inside one trace. Its oldest spans drop once it fills, "
            + "and the finding says nothing about what left.",
        max_payload_size: "The largest single request body it will deserialize. Anything larger is "
            + "refused whole, never truncated.",
        ingest_queue_capacity: "Span batches buffered between the listeners and the event loop. A "
            + "full queue pushes back on the sender as an OTLP 503.",
        analysis_queue_capacity: "Batches waiting for detection. A full queue sheds whole batches, "
            + "and a shed batch is silent to whoever sent it.",
        memory_high_water_pct: "The share of memory above which it refuses new spans rather than meet "
            + "the OOM killer. Zero disables the guard entirely.",
        max_retained_findings: "Findings held for the query API. The oldest are evicted past this, so "
            + "an old problem can leave a snapshot without anyone fixing it.",
        max_export_findings: "Findings one export snapshot carries. The quality gate inside that "
            + "snapshot counts those and no others.",
        max_retained_traces: "Span trees kept so an export can draw them. Zero keeps none, and every "
            + "finding in that export opens without a tree.",
        api_enabled: "Whether the query API is served at all. The Hub reads this daemon through it, "
            + "so a run from here needs it on.",
        listen_addr: "Where the OTLP receivers and /metrics bind. An address outside loopback exposes "
            + "both without authentication.",
        json_socket: "Unix socket for native NDJSON ingestion, alongside OTLP.",
        tls_configured: "TLS on the OTLP listeners. The Hub is told whether a certificate and key are "
            + "set, never where they are.",
        ack_enabled: "The daemon's own acknowledgement store. An acknowledged finding stays in the "
            + "data and stops counting against the gate.",
        ack_api_key_set: "Whether the acknowledgement routes require a key. The Hub is told that one "
            + "exists, never what it is.",
        cors_allowed_origins: "Browser origins the query API answers. Empty sends no CORS headers at all.",
        archive_configured: "Whether it writes a report archive per window, for a later disclosure to "
            + "read back.",
        correlation_enabled: "Whether the cross-trace correlator runs.",
        correlation_window_ms: "The rolling window over which two findings count as having happened "
            + "together.",
        correlation_lag_threshold_ms: "The largest gap between two findings that still counts as together.",
        correlation_min_co_occurrences: "How many times a pair has to happen before it is reported at all.",
        correlation_min_confidence: "How often the second finding follows the first, as a share of the "
            + "first's own occurrences, before the pair is worth reporting.",
        correlation_max_tracked_pairs: "Cap on tracked pairs. The least frequent are evicted past it, "
            + "and the daemon says so above when that happens.",
        energy_model: "Where the energy figure comes from. Measured means a power backend answered, "
            + "estimated means it was derived from I/O counts.",
        api_version: "The Electricity Maps API version these figures were scored against.",
        emission_factor_type: "Lifecycle counts the whole chain behind the electricity, direct counts "
            + "only what the generation itself emits.",
        temporal_granularity: "How finely grid intensity is resolved in time.",
        electricity_maps: "Whether live grid intensity was fetched. Off means the embedded table was "
            + "used, which is a vintage rather than a reading.",
        per_operation_coefficients: "Whether each operation kind carries its own energy coefficient "
            + "instead of one average across all I/O.",
        use_hourly_profiles: "Whether the hour-by-hour shape of the grid is applied rather than a "
            + "flat average.",
        embodied_per_request_gco2: "Embodied carbon charged per request, the manufacture share of "
            + "the figures rather than the electricity.",
        network_energy_per_byte_kwh: "A coefficient the engine deprecated and no longer applies, "
            + "published for configurations that still set it."
    };

    /**
     * The export serialises four thresholds under different names than the file
     * keys the launcher's knobs carry. The same settings seen from the other
     * side, so they take the same sentences rather than a second copy of them.
     */
    const DETECT_ALIAS = {
        n_plus_one_threshold: "n_plus_one_min_occurrences",
        window_ms: "window_duration_ms",
        slow_threshold_ms: "slow_query_threshold_ms",
        slow_min_occurrences: "slow_query_min_occurrences"
    };

    /** The sentence for one setting, or none. */
    function settingCopy(name) {
        return DAEMON_COPY[name] || DETECTION_COPY[DETECT_ALIAS[name] || name] || null;
    }

    /**
     * Mirrors the daemon monitor's own colouring, so the two surfaces agree. An
     * unknown kind stays muted rather than dressed as an alarm: a kind this Hub
     * predates is not automatically bad news.
     */
    const DAEMON_HINT_TONE = {
        ingestion_drops: "crit",
        tuning: "warn",
        cold_start: "muted",
        snapshot_scope: "info"
    };

    /**
     * A real button and not a clickable row: Enter, Space, the focus ring and the
     * semantics come free, and text stays selectable.
     */
    function daemonNameCell(source, index) {
        const button = el("button", {
            type: "button",
            class: "row-toggle",
            "aria-expanded": state.daemonOpen[source.id] === true ? "true" : "false",
            "aria-controls": "daemon-detail-" + index
        }, [el("span", {text: source.name})]);
        button.addEventListener("click", function () {
            toggleDaemon(source, button, index);
        });
        return el("td", {class: "table-strong"}, [button]);
    }

    /**
     * Folded in place rather than through render(), which would rebuild the
     * wrapper and reset its horizontal scroll. Fetched once and kept: a settings
     * snapshot is not a live figure, and refetching on every fold would hit the
     * daemon for a fold rather than for a question.
     */
    function toggleDaemon(source, button, index) {
        const open = button.getAttribute("aria-expanded") !== "true";
        button.setAttribute("aria-expanded", open ? "true" : "false");
        state.daemonOpen[source.id] = open;
        saveFolds();
        const cell = document.getElementById("daemon-detail-" + index);
        if (!cell) return;
        cell.parentNode.hidden = !open;
        // A folded row is not being read, so it stops costing the daemon anything.
        if (!open) {
            stopTicker(source.id);
            return;
        }
        const view = state.daemonViews[source.id];
        if (view === "loading") return;
        // An error is not kept as an answer. The row re-reads itself on its
        // interval, and reopening the fold asks again straight away.
        if (view !== undefined && !view.error_code) {
            startTicker(source, index);
            return;
        }
        loadDaemon(source, index);
    }

    /** The first read of a daemon row, and the retry after a failed one. */
    function loadDaemon(source, index) {
        state.daemonViews[source.id] = "loading";
        const cell = document.getElementById("daemon-detail-" + index);
        if (cell) cell.replaceChildren(daemonPanel(source, index));
        getJson("/api/sources/" + encodeURIComponent(source.id) + "/daemon")
            .then(function (view) {
                state.daemonViews[source.id] = view;
                state.daemonReadAt[source.id] = Date.now();
                state.daemonFullReadAt[source.id] = Date.now();
            })
            .catch(function (error) {
                // The gate's 503 is the Hub being briefly full, not the Hub failing:
                // it clears in about a second and deserves its own sentence.
                state.daemonViews[source.id] = {
                    error_code: /answered 503$/.test(String(error && error.message)) ? "hub_busy" : "internal"
                };
            })
            .finally(function () {
                // The table may have been rebuilt while this was in flight, so the
                // cell is found again rather than kept in a closure.
                const target = document.getElementById("daemon-detail-" + index);
                if (target) target.replaceChildren(daemonPanel(source, index));
                // And the row may have been folded in the meantime, in which case
                // starting to poll it would contradict the fold. A failed read still
                // gets a ticker: its panel carries the same countdown, and the row
                // recovers on its own rather than waiting to be refolded.
                if (state.daemonOpen[source.id]) startTicker(source, index);
            });
    }

    function daemonPanel(source, index) {
        const view = state.daemonViews[source.id];
        if (view === "loading" || view === undefined) {
            return el("div", {class: "daemon-panel"}, [
                el("p", {class: "daemon-loading", role: "status", text: "Reading " + source.name + "."}),
                el("div", {class: "skeleton", style: "height:150px"})
            ]);
        }
        if (view.error_code) return daemonError(source, index, view.error_code);

        return el("div", {class: "daemon-panel"}, [
            el("p", {class: "overline daemon-audience", text: "// intended for devops"}),
            el("p", {
                class: "daemon-source-note",
                text: "Reported by this daemon over its query API. The Hub relays it and verifies none of "
                    + "it. Everything here is read-only, and every setting below is changed where the daemon "
                    + "is deployed, in its Helm values or its own configuration file, never from this Hub."
            }),
            el("div", {id: "daemon-top-" + index}, [daemonTopRow(source, view, index)]),
            terminalBlock({
                head: "// the same view in your terminal",
                sub: "The same figures, plus the tabs this screen leaves out.",
                id: "monitor-command-" + index,
                spell: function (shellId) {
                    return PSL.monitorCommand(source, refreshSeconds(source.id), shellId);
                },
                // Folded by default: the row is opened to read the gauges, and this is
                // the other way of reading them rather than part of that answer.
                fold: {
                    open: state.daemonTerminalOpen[source.id] === true,
                    onToggle: function (open) {
                        state.daemonTerminalOpen[source.id] = open;
                        saveFolds();
                    }
                },
                copyLabel: "Copy the monitor command for " + source.name,
                notes: [
                    "`query monitor` carries the energy and carbon breakdown this screen only summarises. "
                    + "It re-reads on the interval chosen above, and this line changes with it, so the "
                    + "terminal and this row never disagree about how often the daemon is asked. Set that "
                    + "to off and the command drops `--refresh`, leaving the engine its own default of "
                    + "five seconds.",
                    engineNote(),
                    source.auth_header_name
                        ? "The Hub reaches this daemon with an auth header it holds and does not disclose. "
                        + "`query monitor` takes no such flag, so this command works only from somewhere "
                        + "that can reach the daemon directly."
                        : null
                ]
            }),
            settingsDisclosure(source, view)
        ]);
    }

    /**
     * A failed read is not a dead end: the row keeps asking on the same interval
     * the healthy rows use, and the first answer replaces this with the daemon.
     */
    function daemonError(source, index, code) {
        return el("div", {class: "daemon-panel"}, [
            el("div", {class: "banner", "data-tone": "crit"}, [
                critGlyph(16),
                el("div", {}, [
                    el("p", {}, [
                        el("span", {text: "Reading this daemon's settings returned "}),
                        el("span", {class: "code-inline", text: code}),
                        el("span", {
                            text: ": " + (PSL.READ_ERRORS[code] || PSL.ERRORS[code] || "the Hub could not reach it.")
                        })
                    ]),
                    el("p", {
                        class: "notice-sub",
                        text: "The row above still shows the last collection state, which is a different "
                            + "observation made at a different time."
                    })
                ])
            ]),
            el("p", {
                class: "daemon-lead",
                text: "This row asks again on its own, and shows the daemon as soon as it answers."
            }),
            refreshControl(source, index)
        ]);
    }

    const DAEMON_VERDICT = {
        ok: ["nominal", "ok"],
        near_capacity: ["near capacity", "warn"],
        advised: ["advised", "warn"],
        unknown: ["not measurable", "muted"]
    };

    /**
     * What the fold opens on: the verdict, the gauges, and the daemon's own
     * hints beside them rather than under them. The settings are the long part
     * and stay behind one more click.
     */
    function pct(gauge) {
        return gauge ? gauge.pct : null;
    }

    function daemonTopRow(source, view, index) {
        const verdict = DAEMON_VERDICT[view.state] || DAEMON_VERDICT.unknown;
        // Read once and cleared: the badges belong to the read that produced them,
        // not to every rebuild that happens to come after it.
        const moves = state.daemonMoves[source.id] || {};
        delete state.daemonMoves[source.id];
        const main = el("div", {class: "daemon-top-main"}, [
            el("div", {class: "sink-head"}, [
                titledOverline("// right now", "Read from the daemon on the interval below. A tick asks "
                    + "the daemon for its status alone, and once a minute the full export runs to refresh "
                    + "the hints, so the interval prices a small read, not the heavy one."),
                el("span", {class: "sink-sub refresh-read", id: "refresh-read-" + index})
            ]),
            refreshControl(source, index),
            countStrip([
                [gaugeText(view.traces), "active traces", PSL.gaugeTone(pct(view.traces)), moves.traces],
                [gaugeText(view.analysis_queue), "analysis queue",
                    PSL.gaugeTone(pct(view.analysis_queue)), moves.analysis_queue],
                [gaugeText(view.findings), "findings stored",
                    PSL.gaugeTone(pct(view.findings)), moves.findings],
                // No cap and only one direction: an uptime that grows every read is
                // not news, and a tone would say it is running out of something. Down to
                // the minute, because two units hide a whole day: a daemon up for 10 d
                // 23 h reads the same as one up for 10 d flat.
                [view.uptime_seconds == null ? "unknown" : PSL.durMinutes(view.uptime_seconds * 1000), "uptime"]
            ]),
            el("p", {
                class: "daemon-lead",
                text: "Each figure is shown against the cap it runs into. Being near one is not a problem "
                    + "by itself, and this screen does not decide that it is: the daemon does, from counters "
                    + "the Hub cannot see."
            })
        ]);

        const side = el("div", {class: "daemon-top-side"}, [
            el("div", {class: "sink-head"}, [
                titledOverline("// what the daemon recommends", "These come from counters inside the "
                    + "daemon that no report and no dashboard carries. The Hub relays the sentences and "
                    + "writes none of its own."),
                el("span", {class: "sink-sub", text: "Written by the daemon, not by the Hub."})
            ])
        ]);
        if (view.hints_unavailable_reason) {
            // An unread export is not a clean bill: silence has to be earned.
            side.appendChild(proseInto(el("p", {class: "daemon-lead"}),
                "The export this screen reads hints from could not be read: `"
                + view.hints_unavailable_reason + "`. Whatever the daemon recommends right now is "
                + "unknown, which is not the same thing as nothing."));
        } else if (view.warnings.length === 0) {
            side.appendChild(el("p", {
                class: "daemon-lead",
                text: "Nothing. The daemon emits a hint when its own counters show a setting is undersized "
                    + "for the load it is taking. Silence here means those counters were clean at the "
                    + "instant it was read, not that the settings are right."
            }));
        } else {
            view.warnings.forEach(function (hint) {
                side.appendChild(el("div", {class: "outcome-warning"}, [
                    el("span", {
                        class: "outcome-warning-kind",
                        "data-tone": DAEMON_HINT_TONE[hint.kind] || "muted",
                        text: hint.kind
                    }),
                    proseInto(el("span", {class: "outcome-warning-message"}), hint.message)
                ]));
            });
            if (view.warnings_dropped > 0) {
                side.appendChild(el("p", {
                    class: "daemon-lead",
                    text: view.warnings_dropped + " more arrived than the Hub relays in one view."
                }));
            }
        }

        return el("div", {}, [
            el("div", {class: "daemon-verdict-row"}, [
                el("span", {class: "daemon-verdict", "data-tone": verdict[1], text: verdict[0]})
            ]),
            el("div", {class: "daemon-top"}, [main, side])
        ]);
    }

    /**
     * How often this row re-reads the daemon, and how long until it does.
     *
     * The same knob `query monitor --refresh` carries, with an off position the
     * command does not have: a terminal session is opened to watch, a table row
     * is often opened just to read a setting once.
     */
        // Declared in seconds, not milliseconds: the engine's `--refresh` takes whole
        // seconds and the printed command mirrors this list, so a choice that is not
        // one would drop the flag while the row kept re-reading. Deriving the
        // milliseconds from the seconds makes that impossible rather than unlikely.
    const REFRESH_SECONDS = [0, 5, 10, 30, 60];
    const REFRESH_CHOICES = REFRESH_SECONDS.map(function (seconds) {
        return [seconds * 1000, seconds ? seconds + " s" : "off"];
    });
    const DEFAULT_REFRESH_MS = 5000;

    function refreshMs(sourceId) {
        const chosen = state.daemonRefreshMs[sourceId];
        return chosen === undefined ? DEFAULT_REFRESH_MS : chosen;
    }

    function refreshSeconds(sourceId) {
        return refreshMs(sourceId) / 1000;
    }

    function refreshControl(source, index) {
        const ms = refreshMs(source.id);
        const select = el("select", {class: "refresh-select", "aria-label": "Re-read interval"});
        REFRESH_CHOICES.forEach(function (choice) {
            const option = el("option", {value: String(choice[0]), text: choice[1]});
            if (choice[0] === ms) option.selected = true;
            select.appendChild(option);
        });
        // Which device put the focus there, for the ring rule in the stylesheet.
        select.addEventListener("pointerdown", function () {
            select.dataset.pointer = "true";
        });
        select.addEventListener("keydown", function () {
            delete select.dataset.pointer;
        });
        select.addEventListener("blur", function () {
            delete select.dataset.pointer;
        });
        select.addEventListener("change", function () {
            state.daemonRefreshMs[source.id] = Number(select.value);
            startTicker(source, index);
            // Only the line is rewritten: the note under it is worded to hold at any
            // interval, including off, so it never needs to be.
            const printed = document.getElementById("monitor-command-" + index);
            if (printed) {
                printed.textContent = PSL.monitorCommand(source, refreshSeconds(source.id), state.shell);
            }
        });
        const ring = refreshRing();
        if (ms) ring.querySelector(".refresh-ring-fill").style.setProperty("--cycle", ms + "ms");

        // Its own line under the heading, not squeezed beside it. The ring is
        // decoration, the sentence is the information: under reduced motion the
        // ring stops moving and the countdown still counts.
        return el("div", {class: "refresh"}, [
            ring,
            el("span", {class: "refresh-next", id: "refresh-next-" + index, role: "status"}),
            el("span", {class: "refresh-label", text: "every"}),
            select
        ]);
    }

    /**
     * The cycle as a filling disc. It sweeps once per interval and carries on
     * into the next one: the sweep is timed from the cycle's own start rather
     * than from the last read, so the network time of a read does not show up as
     * a pause at full.
     */
    function refreshRing() {
        const node = document.createElementNS("http://www.w3.org/2000/svg", "svg");
        node.setAttribute("viewBox", "0 0 24 24");
        node.setAttribute("width", "14");
        node.setAttribute("height", "14");
        node.setAttribute("aria-hidden", "true");
        node.setAttribute("class", "refresh-ring");

        const track = document.createElementNS("http://www.w3.org/2000/svg", "circle");
        track.setAttribute("cx", "12");
        track.setAttribute("cy", "12");
        track.setAttribute("r", "11");
        track.setAttribute("class", "refresh-ring-track");
        node.appendChild(track);

        // A solid disc drawn as one stroked circle: at half the radius with a
        // stroke as thick as the diameter, the stroke covers the whole disc, so
        // the dash sweeps a filled wedge rather than an outline. r 5.25 puts the
        // wedge's outer edge exactly on the track's inner edge instead of over
        // it, and pathLength lets the stylesheet count in percent.
        const fill = document.createElementNS("http://www.w3.org/2000/svg", "circle");
        fill.setAttribute("cx", "12");
        fill.setAttribute("cy", "12");
        fill.setAttribute("r", "5.25");
        fill.setAttribute("pathLength", "100");
        fill.setAttribute("class", "refresh-ring-fill");
        node.appendChild(fill);
        return node;
    }

    /**
     * One second, one job: it writes the countdown and fires the read when the
     * time is up. Deriving the deadline from the last read rather than counting
     * down a variable means a slow tab cannot make the interval drift.
     */
    function startTicker(source, index) {
        stopTicker(source.id);
        restartCycle(source, index);
        tickRefresh(source, index);
        if (!refreshMs(source.id)) return;
        state.daemonTickers[source.id] = setInterval(function () {
            tickRefresh(source, index);
        }, 1000);
    }

    /**
     * The sweep is one looping CSS animation whose duration is the interval, not
     * a value written every second: a per-second write can only land on whole
     * seconds, so the last slice of the disc never got drawn before the cycle
     * turned over. Restarted only to resynchronise it with a read.
     */
    function restartSweep(index, ms) {
        const host = document.getElementById("refresh-next-" + index);
        const fill = host && host.parentNode.querySelector(".refresh-ring-fill");
        if (!fill) return;
        if (!ms) {
            fill.style.animation = "none";
            return;
        }
        fill.style.animation = "none";
        fill.style.setProperty("--cycle", ms + "ms");
        void fill.getBoundingClientRect();
        fill.style.animation = "";
    }

    /** The disc and the countdown clock restart together, from one place. */
    function restartCycle(source, index) {
        const ms = refreshMs(source.id);
        state.daemonCycleAt[source.id] = Date.now();
        restartSweep(index, ms);
        // The disc and the read now share one deadline instead of one starting the
        // other: the read lands when the sweep closes, not on the beat after it.
        clearTimeout(state.daemonCycleTimers[source.id]);
        if (!ms) return;
        state.daemonCycleTimers[source.id] = setTimeout(function () {
            refreshDaemon(source, index);
        }, ms);
    }

    function stopTicker(sourceId) {
        clearInterval(state.daemonTickers[sourceId]);
        delete state.daemonTickers[sourceId];
        clearTimeout(state.daemonCycleTimers[sourceId]);
        delete state.daemonCycleTimers[sourceId];
    }

    function stopAllTickers() {
        // Both maps: a source can hold a cycle timer without holding a ticker, and
        // walking only the tickers would leave that one running past the screen.
        new Set(Object.keys(state.daemonTickers).concat(Object.keys(state.daemonCycleTimers)))
            .forEach(stopTicker);
    }

    function tickRefresh(source, index) {
        const read = document.getElementById("refresh-read-" + index);
        const next = document.getElementById("refresh-next-" + index);
        // The countdown is the ticker's only requirement. A failed row carries one
        // and no read age, having never had a read to age.
        if (!next) {
            stopTicker(source.id);
            return;
        }

        if (read) {
            const readAt = state.daemonReadAt[source.id] || Date.now();
            read.textContent = "Read " + PSL.dur(Date.now() - readAt) + " ago.";
        }

        const ms = refreshMs(source.id);
        // The sweep runs on its own clock, set by startTicker and refreshDaemon.
        // Timing it from the last read would hold the disc at full for however
        // long the request takes, which reads as a stall rather than as a cycle.
        const cycleAt = state.daemonCycleAt[source.id];
        const ring = next.parentNode.querySelector(".refresh-ring");
        const fill = ring && ring.querySelector(".refresh-ring-fill");
        if (!ms) {
            next.textContent = "Not re-reading.";
            if (ring) ring.hidden = true;
            return;
        }
        if (ring) ring.hidden = false;
        const left = Math.max(0, cycleAt + ms - Date.now());
        next.textContent = "Next in " + Math.ceil(left / 1000) + " s.";
    }

    /**
     * Re-reads and replaces the state block only. A daemon's settings do not
     * change without a restart, so rebuilding those every few seconds would
     * throw away the reader's open groups and their focus for nothing.
     */
        // A light tick costs the daemon one status read. The full read, which
        // buffers the export to refresh the hints, runs at most this often.
    const FULL_READ_EVERY_MS = 60000;

    /**
     * Swaps a rebuilt block in, unless the reader is inside it: rebuilding would
     * close the interval select under their pointer. The next tick catches up.
     *
     * @param {string} id the block to replace
     * @param {() => Node} build called only when the swap is going ahead, since
     *   building a row has side effects the discarded copy would swallow
     * @returns {boolean} whether the swap happened
     */
    function replaceIfIdle(id, build) {
        const host = document.getElementById(id);
        if (!host || host.contains(document.activeElement)) return false;
        // Built here rather than by the caller: daemonTopRow consumes the move
        // badges as it renders them, and building a row that is then discarded
        // would spend that read's badges on nothing.
        host.replaceChildren(build());
        return true;
    }

    function refreshDaemon(source, index) {
        // A folded row is not being read. The cycle timer can fire in the very gap
        // between the fold clearing it and this call arming the next one, and that
        // one would then re-arm itself for good, on a row nobody is looking at.
        if (state.daemonOpen[source.id] !== true) return;
        // A separate flag rather than a sentinel in daemonViews: a render during
        // the flight reads daemonViews, and a string there would crash it.
        if (state.daemonInFlight[source.id]) {
            // Slower than its own interval. Wait a whole one rather than queue up
            // behind it, and keep the cycle armed so the row does not go quiet.
            restartCycle(source, index);
            return;
        }
        state.daemonInFlight[source.id] = true;
        restartCycle(source, index);
        const previous = state.daemonViews[source.id];
        // A light read is only ever an overlay on a full one, so a row with
        // nothing to lay it over asks the cheap question first, and only reads in
        // full once it knows there is something to read.
        const plan = PSL.refreshPlan(
            previous,
            Date.now() - (state.daemonFullReadAt[source.id] || 0),
            FULL_READ_EVERY_MS);
        // Read from the plan rather than judged a second time: the plan only says
        // "light" for a view that can be merged onto, so the two cannot disagree.
        const kept = plan === "light" ? previous : null;
        getJson("/api/sources/" + encodeURIComponent(source.id) + "/daemon"
            + (plan === "full" ? "" : "?refresh=status"))
            .then(function (view) {
                if (plan === "probe") {
                    // The daemon answers again: only a full read renders it, and that is
                    // the same read this row started with, skeleton and all.
                    if (!view.error_code) return loadDaemon(source, index);
                    // Still down, and possibly down differently: the panel is replaced
                    // only when the reason changed.
                    const stale = previous.error_code !== view.error_code;
                    state.daemonViews[source.id] = view;
                    if (stale) replaceIfIdle("daemon-detail-" + index, function () {
                        return daemonPanel(source, index);
                    });
                    return;
                }
                // A transient error does not outrank data: the last good reading is
                // kept and its age keeps counting, exactly as a dropped connection is
                // handled below.
                if (view.error_code) return;
                if (plan === "light") view = PSL.mergeLight(kept, view);
                // Against what was on screen a moment ago, so the badge answers "what
                // changed while I was looking at it" rather than comparing two reads
                // the reader never saw next to each other.
                state.daemonMoves[source.id] = {
                    traces: PSL.gaugeMove(kept && kept.traces, view.traces),
                    analysis_queue: PSL.gaugeMove(kept && kept.analysis_queue, view.analysis_queue),
                    findings: PSL.gaugeMove(kept && kept.findings, view.findings)
                };
                state.daemonViews[source.id] = view;
                state.daemonReadAt[source.id] = Date.now();
                if (plan === "full") state.daemonFullReadAt[source.id] = Date.now();
            })
            .catch(function () {
                // Keep the last good reading rather than blanking the panel, and let
                // its age say how stale it now is.
                state.daemonViews[source.id] = previous;
            })
            .finally(function () {
                delete state.daemonInFlight[source.id];
                const view = state.daemonViews[source.id];
                if (!view || view === "loading" || view.error_code) return;
                if (!replaceIfIdle("daemon-top-" + index, function () {
                    return daemonTopRow(source, view, index);
                })) return;
                tickRefresh(source, index);
            });
    }

    function gaugeText(gauge) {
        if (!gauge || gauge.value == null) return "unknown";
        if (gauge.capacity == null) return group(gauge.value);
        return group(gauge.value) + " / " + group(gauge.capacity);
    }

    /**
     * The settings, behind one click. They are the long part of the view and the
     * least urgent: an operator opens this row to see whether the daemon is
     * coping, and reads the settings once it is not.
     */
    function settingsDisclosure(source, view) {
        const open = state.daemonSettingsOpen[source.id] === true;
        const count = view.config ? Object.keys(view.config).length : 0;
        const cards = el("div", {class: "settings-cards"},
            settingsColumns(settingsCards(source.id, view)));
        // The preamble sits above the columns rather than inside them, or it would
        // flow into the first one as if it were a card.
        const body = el("div", {},
            [view.config ? settingsPreamble(view) : configAbsenceNote(view), cards]);
        body.hidden = !open;

        const button = el("button", {
            type: "button",
            class: "settings-more",
            "aria-expanded": open ? "true" : "false"
        }, [el("span", {
            text: count > 0
                ? "The " + count + " applied settings, and what this daemon changed"
                : "What this daemon detects with"
        })]);
        button.addEventListener("click", function () {
            const next = button.getAttribute("aria-expanded") !== "true";
            button.setAttribute("aria-expanded", next ? "true" : "false");
            state.daemonSettingsOpen[source.id] = next;
            saveFolds();
            body.hidden = !next;
        });
        return el("div", {class: "settings-block"}, [button, body]);
    }

    // Fixed, and not measured: the table around this sets min-width: 1090px and
    // scrolls rather than shrink, so the room here never falls below three.
    const SETTINGS_COLUMNS = 3;

    /**
     * Deals the cards into independent columns, round-robin so they still read
     * left to right. Not a grid: a grid row is as tall as its tallest card, so
     * opening one card pushed down every card in the rows below it, across all
     * columns. Here a card only ever moves the cards under it in its own column.
     * Not CSS columns either, which repack by height and so re-shuffled cards
     * sideways under the reader's own click.
     */
    function settingsColumns(cards) {
        // Never more columns than cards: an empty one still takes its share of the
        // width, so three of them would render a lone card at a third of the row.
        const count = Math.min(SETTINGS_COLUMNS, cards.length);
        const columns = [];
        for (let index = 0; index < count; index++)
            columns.push(el("div", {class: "settings-col"}));
        cards.forEach(function (card, index) {
            columns[index % count].appendChild(card);
        });
        return columns;
    }

    /**
     * Why no settings are shown, as prose above the cards rather than dealt into
     * a column as if it were one: a paragraph in a third of the row reads as a
     * card that lost its rows.
     */
    function configAbsenceNote(view) {
        const reason = view.config_unavailable_reason;
        return proseInto(el("p", {class: "daemon-lead"}),
            reason === "api_disabled"
                ? "This daemon does not serve its configuration: `api_enabled` is off in its own "
                + "`[daemon]` section. Everything above came from the export instead."
                : reason === "unreadable"
                    ? "This daemon answered for its configuration with something the Hub could not "
                    + "relay, an error status or a body that is not the `[daemon]` object. The gauges "
                    + "above are the same daemon answering fine."
                    : "This daemon did not answer for its configuration, so none is shown rather than a "
                    + "copy from some earlier moment.");
    }

    function settingsCards(sourceId, view) {
        const cards = [];
        if (view.config) {
            DAEMON_GROUPS.forEach(function (spec) {
                const present = spec[2].filter(function (name) {
                    return view.config[name] !== undefined;
                });
                if (present.length > 0) {
                    cards.push(settingsCard(
                        sourceId, spec[0], spec[1], present, view.config, view.daemon_defaults));
                }
            });
        }
        if (view.detection_config) {
            cards.push(settingsCard(
                sourceId,
                "// detection thresholds",
                "What counts as a problem. The same knobs the launcher lets a run override on a backend.",
                Object.keys(view.detection_config),
                view.detection_config,
                view.detection_defaults));
        }
        if (view.scoring_config || view.energy_model) {
            const scoring = Object.assign({}, view.scoring_config || {});
            if (view.energy_model) scoring.energy_model = view.energy_model;
            cards.push(settingsCard(
                sourceId,
                "// carbon scoring",
                "Where the energy figures come from, not what they are.",
                Object.keys(scoring),
                scoring,
                null));
        }
        return cards;
    }

    /**
     * What the marking means, and what it is worth. The defaults belong to the
     * binary this Hub embeds, which is the same approximation the daemon's own
     * monitor makes against the binary running it, so the version is named
     * rather than assumed away.
     */
    function settingsPreamble(view) {
        const note = proseInto(el("p", {class: "settings-preamble"}),
            "A value this daemon changed is marked, with the engine's default beside it. Compared "
            + "against perf-sentinel `" + view.defaults_engine_version + "`, the binary this Hub "
            + "embeds. It covers the `[daemon]` and `[detection]` sections and the scoring half of "
            + "`[green]`. The gate thresholds under `[thresholds]` are not published as a section, so "
            + "a value set there is real and simply not visible here.");
        if (view.version && view.version !== view.defaults_engine_version) {
            proseInto(
                note.appendChild(el("span", {class: "settings-preamble-skew"})),
                " This daemon runs `" + view.version + "`, so a default could have moved between the two "
                + "and a value marked as changed may only be a different default.");
        }
        return note;
    }

    /**
     * One group, folded. Folded is the useful state: eight headings fit on a
     * glance, and the count of departures on each says which one to open. The
     * gloss lives in the body rather than the heading, or the folded list would
     * be as long as the open one.
     */
    function settingsCard(sourceId, head, sub, names, config, defaults) {
        const key = sourceId + "|" + head;
        const open = state.daemonGroupOpen[key] === true;
        const changed = names.filter(function (name) {
            return isChanged(name, config[name], defaults);
        }).length;

        const heading = el("span", {class: "overline", text: head});
        const button = el("button", {
            type: "button",
            class: "settings-card-head",
            "aria-expanded": open ? "true" : "false"
        }, [
            heading,
            el("span", {class: "settings-card-n", text: String(names.length)}),
            changed > 0
                ? el("span", {class: "settings-card-changed", text: changed + " changed"})
                : null
        ]);

        const body = el("div", {class: "settings-card-body"}, [
            el("p", {class: "sink-sub settings-card-sub", text: sub})
        ]);
        body.hidden = !open;
        const rows = el("dl", {class: "settings-rows"});
        names.forEach(function (name) {
            rows.appendChild(settingRow(name, config[name], defaults));
        });
        body.appendChild(rows);

        button.addEventListener("click", function () {
            const next = button.getAttribute("aria-expanded") !== "true";
            button.setAttribute("aria-expanded", next ? "true" : "false");
            state.daemonGroupOpen[key] = next;
            saveFolds();
            body.hidden = !next;
        });
        return el("section", {class: "settings-card"}, [button, body]);
    }

    /** A value that departs from the engine's default, when one is known. */
    function isChanged(name, value, defaults) {
        if (!defaults) return false;
        const fallback = defaults[name];
        return fallback !== undefined && JSON.stringify(value) !== JSON.stringify(fallback);
    }

    function settingRow(name, value, defaults) {
        const fallback = defaults ? defaults[name] : undefined;
        const changed = isChanged(name, value, defaults);
        const row = el("div", {class: "setting"}, [
            el("dt", {class: "setting-k", text: name}),
            settingValue(name, value, changed)
        ]);
        if (changed) {
            row.appendChild(el("dd", {
                class: "setting-default",
                text: "default " + daemonValue(name, fallback)
            }));
        }
        const copy = settingCopy(name);
        if (copy) row.appendChild(el("dd", {class: "setting-note", text: copy}));
        return row;
    }

    function settingValue(name, value, changed) {
        if (name === "environment") {
            return el("dd", {class: "setting-v", "data-changed": changed ? "true" : null}, [
                el("span", {class: "chip chip-declared", text: String(value)})
            ]);
        }
        return el("dd", {
            class: "setting-v",
            "data-changed": changed ? "true" : null,
            text: daemonValue(name, value)
        });
    }

    function daemonValue(name, value) {
        if (value === null) return "(not set)";
        if (name === "sampling_rate" || name === "correlation_min_confidence") return share(value);
        // Zero is not a percentage here, it switches the guard off entirely.
        if (name === "memory_high_water_pct") return value === 0 ? "off" : value + " %";
        if (name === "max_payload_size") return PSL.bytes(value);
        if (Array.isArray(value)) return value.length === 0 ? "(none)" : value.join(", ");
        if (name === "tls_configured" || name === "archive_configured") {
            return value ? "configured" : "not configured";
        }
        if (name === "ack_api_key_set") return value ? "set" : "unset";
        // The name says _ms, so the millisecond figure stays primary: it is the one
        // that goes back into the file. PSL.dur rounds to the second, so the
        // readable form only appears once there is a second to read.
        if (/_ms$/.test(name) && typeof value === "number") {
            return group(value) + " ms" + (value >= 1000 ? " (" + PSL.dur(value) + ")" : "");
        }
        if (typeof value === "boolean") return value ? "yes" : "no";
        if (typeof value === "number") return group(value);
        return String(value);
    }

    /**
     * A 0-to-1 share as a percentage, never rounded down to a zero it is not. A
     * sampling rate of 0.0005 is one trace in two thousand, which is very
     * different from none at all.
     */
    function share(value) {
        if (typeof value !== "number") return String(value);
        const pct = value * 100;
        if (value > 0 && pct < 0.1) return String(value);
        return (Math.round(pct * 10) / 10) + " %";
    }


    // ---------------------------------------------------- screen: new analysis

    const QUICK_RANGES = [
        "15m", "30m", "1h", "3h", "6h", "12h", "24h", "2d", "7d", "30d", "90d", "180d"
    ];

    function selectedSource() {
        return (state.sources || []).find(function (source) {
            return source.id === state.form.sourceId;
        }) || null;
    }

    /** Changing source clears both acknowledgements and closes the picker: they
     were answers about a different source. */
    function selectSource(id) {
        state.form.sourceId = id;
        saveSource(id);
        state.form.ackUnreachable = false;
        state.form.ackHeavy = false;
        state.form.pickerOpen = false;
        state.form.detection = {};
        render();
    }

    function setMode(mode) {
        state.form.mode = mode;
        // Switching clears the other field, and a trace ID takes no window at all,
        // so the picker cannot stay open behind a hidden control.
        if (mode === "trace") {
            state.form.service = "";
            state.form.pickerOpen = false;
        } else {
            state.form.traceId = "";
        }
        render();
    }

    /**
     * Updated in place, never re-rendered. A full render replaces the range and
     * the number field mid-interaction, which drops the browser's pointer
     * capture (the handle stops following the mouse) and the text caret.
     */
    function setMaxTraces(value) {
        state.form.maxTraces = value;
        // Dropping back below the ceiling withdraws the question that was asked
        // about it.
        if (!PSL.weightBand(value, tracesCap()).needsAck) state.form.ackHeavy = false;
        refreshTraces();
        updateSubmit();
    }

    /** Everything on screen that reads maxTraces, refreshed without a re-render. */
    function refreshTraces() {
        const cap = tracesCap();
        const band = PSL.weightBand(state.form.maxTraces, cap);
        const over = band.key === "over" || band.key === "invalid";
        const value = String(state.form.maxTraces);

        const number = document.getElementById("traces-number");
        const slider = document.getElementById("traces-slider");
        // Assigned only when it differs, so the element the operator is dragging or
        // typing into is left alone.
        if (number && number.value !== value) number.value = value;
        if (number) {
            number.toggleAttribute("data-over", over);
            number.setAttribute("data-band", band.key);
        }
        if (slider) {
            const clamped = String(Math.min(Math.max(state.form.maxTraces, 1), cap));
            if (slider.value !== clamped) slider.value = clamped;
        }

        const chip = document.getElementById("traces-band");
        if (chip) {
            chip.textContent = band.label;
            chip.setAttribute("style", bandStyle(band));
        }

        const note = document.getElementById("traces-cap");
        if (note) {
            note.textContent = capNote(band, cap);
            note.setAttribute("data-over", over ? "true" : "false");
        }

        const body = document.getElementById("traces-body");
        if (body) {
            body.textContent = band.body;
            body.setAttribute("style", "color:" + band.fg);
        }

        const slot = document.getElementById("traces-ack");
        if (!slot) return;
        if (band.needsAck) slot.replaceChildren(heavyAck());
        else slot.replaceChildren();
    }

    function renderNewScreen() {
        const section = el("section", {}, [
            ruledOverline("// new analysis"),
            el("h1", {class: "page-title", text: "Run an analysis"})
        ]);

        if (state.loading) {
            section.appendChild(el("div", {class: "new-grid"}, [
                el("div", {class: "card skeleton", style: "height:280px"}),
                el("div", {class: "card skeleton", style: "height:280px"})
            ]));
            return section;
        }
        if (state.sourcesError) {
            section.appendChild(hubUnreachableBanner());
            return section;
        }
        if (!state.sources || state.sources.length === 0) {
            section.appendChild(el("div", {class: "empty-state", text: "This Hub has no configured source."}));
            return section;
        }

        const source = selectedSource();
        const skew = source && PSL.skew(source.producer_version);
        const right = el("div", {class: "new-column"}, [parametersPanel(), costBand()]);
        const advanced = source && source.kind !== "daemon" ? advancedPanel() : null;
        if (advanced) right.appendChild(advanced);
        if (skew) right.appendChild(skewNotice(source, skew));
        if (source && !source.reachable) right.appendChild(unreachableNotice(source));
        right.appendChild(submitRow());
        // Last, deliberately. The button is the action, and the other way of doing
        // the same thing is read after the decision rather than against it.
        right.appendChild(terminalSlot());

        section.appendChild(el("div", {class: "new-grid"}, [sourcePanel(), right]));
        return section;
    }

    /**
     * Rebuilt in place on every keystroke rather than through render(), which
     * would take the focus out of the field being typed into.
     */
    function terminalSlot() {
        // No build queued here: submitRow's updateSubmit runs after the same
        // render and builds the panels, so a second build would be thrown away.
        state.terminalSig = null;
        return el("div", {id: "terminal-panels", class: "terminal-stack"});
    }

    /**
     * Rebuilt only when what it would say changed. This runs on every keystroke
     * through updateSubmit, and the command plus the overrides file are a
     * complete signature of the panels: every conditional note keys off one of
     * them or off the source, which the signature carries too.
     */
    function refreshTerminal() {
        const slot = document.getElementById("terminal-panels");
        if (!slot) return;
        const source = selectedSource();
        const sig = source
            ? source.id + "|" + (PSL.analysisCommand(source, buildRequest(source)) || "") + "|"
            + PSL.detectionToml(state.form.detection)
            : "";
        if (sig === state.terminalSig) return;
        state.terminalSig = sig;
        slot.replaceChildren.apply(slot, terminalPanels(source));
    }

    /**
     * The command, and the file it needs when thresholds were changed. Empty for
     * a daemon, which is read over HTTP and has no command line at all.
     */
    function terminalPanels(source) {
        if (!source) return [];
        const request = buildRequest(source);
        // Only to know whether there is a command at all: the block spells its own
        // line per shell, and this one is never the one shown.
        if (!PSL.analysisCommand(source, request, state.shell)) return [];

        const changed = Object.keys(state.form.detection).length;
        const trace = state.form.mode === "trace";
        const panels = [terminalBlock({
            head: "// prefer your terminal?",
            sub: "The same run, spelled out.",
            help: "The Hub runs this same binary. What is missing here is the JSON output and the "
                + "second command that renders it, which exist so the Hub can build a dashboard. "
                + "A terminal does not need either.",
            spell: function (shellId) {
                return PSL.analysisCommand(source, request, shellId);
            },
            copyLabel: "Copy the analysis command",
            // Folded by default: the button above it is the way this Hub is meant to
            // be used, and this is the alternative for whoever wants it.
            fold: {
                open: state.panelOpen.terminal === true,
                onToggle: function (open) {
                    state.panelOpen.terminal = open;
                    saveFolds();
                }
            },
            notes: [
                // What this is, then what it takes to run it. Nobody goes and installs
                // a binary before knowing what the line above them does.
                "This is the same request the button above sends, written as the engine's own "
                + "arguments. It runs wherever perf-sentinel is installed and does not pass through "
                + "this Hub: no worker slot, no queue, and no report kept here for "
                + state.status.limits.report_retention_hours + " hours.",
                "It prints its findings to the terminal. There is no dashboard at the end of it and no "
                + "link to share, which is the trade for not spending a worker.",
                trace
                    ? "An ID resolves to exactly one trace, so the engine takes neither a window nor a "
                    + "trace cap here, exactly as the form above stops offering them."
                    : null,
                stepsBlock([
                    engineNeed(el("span", {})),
                    !trace && !state.form.service.trim()
                        ? "Fill in the service name above. The command carries an empty one as it stands, "
                        + "and the engine refuses it as it stands."
                        : null,
                    source.auth_header_name ? tokenStep(source) : null,
                    changed > 0
                        ? "Put the `perf-sentinel.toml` below next to where you run the command. "
                        + (changed === 1 ? "The threshold you moved has" : "The thresholds you moved have")
                        + " no command-line flag, so the engine reads "
                        + (changed === 1 ? "it" : "them") + " from that file."
                        : null
                ])
                // The note that used to say which shell this was quoted for: the tabs
                // above the line say it, and they let the reader take the other one.
            ]
        })];

        if (changed > 0) {
            panels.push(terminalBlock({
                head: "// perf-sentinel.toml",
                sub: "Only the thresholds you changed.",
                text: PSL.detectionToml(state.form.detection),
                copyLabel: "Copy the perf-sentinel.toml fragment",
                download: "perf-sentinel.toml",
                notes: [
                    "Every threshold this file leaves out keeps the engine's own default, and the Hub only "
                    + "records a value that actually departs from one. A run launched from the button "
                    + "above carries the same numbers, so the two are comparable with each other.",
                    "Put it in the directory you run the command from. The `-c` above makes it required, "
                    + "so a run that cannot find it stops instead of quietly falling back to the "
                    + "engine's own defaults, which are the numbers you just moved away from."
                ]
            }));
        }

        return panels;
    }

    function hubUnreachableBanner() {
        return el("div", {class: "banner", "data-tone": "crit"}, [
            critGlyph(16),
            el("div", {
                text: "The Hub is not answering. This is the Hub itself and not any one source, so nothing "
                    + "can be launched from here until it is back. Reload once it responds again."
            })
        ]);
    }

    function sourcePanel() {
        return el("div", {class: "card source-panel"}, [
            el("div", {class: "panel-head"}, [
                el("span", {class: "overline", text: "// source"}),
                el("span", {class: "panel-head-source", text: state.sources.length + " configured"})
            ]),
            el("div", {class: "source-list", role: "radiogroup", "aria-label": "Source"},
                sourceRows()),
            el("p", {class: "panel-note"}, [
                el("span", {class: "panel-note-rule", "aria-hidden": "true"}),
                el("span", {
                    text: "A dashed outline marks a value the source declares about itself. The Hub never "
                        + "measures it. A misconfigured deployment can label production as staging."
                })
            ])
        ]);
    }

    // Two kinds that behave differently enough to be worth separating: a daemon is
    // polled and pushes on its own, a trace backend is only read when a run asks.
    // The fleet table groups on the same rule, from the same helper.
    function sourceRows() {
        const kinds = PSL.splitByKind(state.sources);
        if (!kinds.split) return state.sources.map(sourceRadio);
        const radios = function (group) {
            return group.map(function (entry) {
                return sourceRadio(entry.source);
            });
        };
        return [sourceGroupLabel("daemons")].concat(
            radios(kinds.daemons), [sourceGroupLabel("trace backends")], radios(kinds.backends));
    }

    // aria-hidden: each row already names its own kind, so the label would only
    // repeat it, and it is not a radio the group should offer.
    function sourceGroupLabel(text) {
        return el("div", {class: "source-group", role: "presentation", "aria-hidden": "true"}, [
            el("span", {class: "overline", text: text}),
            el("span", {class: "source-group-rule"})
        ]);
    }

    function sourceRadio(source) {
        const selected = source.id === state.form.sourceId;
        const now = Date.now();
        const line1 = el("div", {class: "source-line"}, [
            el("span", {class: "source-name", text: source.name}),
            el("span", {class: "health", "data-health": source.reachable ? "ok" : "crit"}, [
                el("span", {class: "health-dot"}),
                el("span", {
                    text: source.reachable
                        ? "reachable"
                        : "unreachable " + PSL.dur(now - source.unreachable_since_ms)
                })
            ])
        ]);

        const line2 = el("div", {class: "source-line source-meta"}, [
            el("span", {class: "chip", text: PSL.KIND_LABEL[source.kind] || source.kind}),
            el("span", {class: "chip chip-declared", text: source.environment}),
            el("span", {class: "source-version", text: producerLabel(source)})
        ]);
        const gap = PSL.skew(source.producer_version);
        if (gap) line2.appendChild(el("span", {class: "skew-pill", "data-dir": gap.dir, text: gap.label}));

        const button = el("button", {
            type: "button",
            class: "source-row",
            role: "radio",
            "aria-checked": selected ? "true" : "false"
        }, [el("span", {class: "source-dot"}), el("span", {}, [line1, line2])]);
        button.addEventListener("click", function () {
            selectSource(source.id);
        });
        return button;
    }

    function producerLabel(source) {
        if (source.producer_version) return "producer " + source.producer_version;
        return source.kind === "daemon" ? "producer unknown" : "no producer version";
    }

    function parametersPanel() {
        const source = selectedSource();
        if (!source) {
            return el("div", {class: "card params-panel"}, [
                el("div", {class: "empty-state", text: "Pick a source to see what it takes."})
            ]);
        }

        const head = el("div", {class: "panel-head"}, [
            el("span", {class: "overline", text: source.kind === "daemon" ? "// parameters" : "// query"}),
            el("span", {class: "panel-head-source", text: source.name})
        ]);

        const panel = el("div", {class: "card params-panel"}, [head]);
        if (source.kind === "daemon") panel.appendChild(daemonNotice());
        else backendControls(source).forEach(function (node) {
            panel.appendChild(node);
        });
        return panel;
    }

    function daemonNotice() {
        return el("div", {class: "notice"}, [
            svg([["circle", {cx: "12", cy: "12", r: "9"}], ["path", {d: "M12 11v5M12 8.2v.2"}]], 16),
            el("div", {}, [
                el("p", {text: "No parameters. A daemon snapshot is whatever it holds in memory right now."}),
                el("p", {
                    class: "notice-sub",
                    text: "The window is the daemon's own ring buffer. There is nothing to widen: asking for "
                        + "three hours from a process that keeps ten minutes would be a request the source "
                        + "cannot answer, so the launcher does not offer it."
                }),
                el("p", {
                    class: "notice-sub",
                    text: "There is no command line for this either. What this daemon is configured with, "
                        + "and what it is holding at this moment, is on the Sources screen, on its own row."
                }),
                sourcesRowLink()
            ])
        ]);
    }

    /** The label promises an open row, so the row arrives open: the sources
     screen reads the flag and runs the first read itself. */
    function sourcesRowLink() {
        const link = el("a", {class: "pill-button pill-sm", href: "#/sources", text: "Open its row on Sources"});
        link.addEventListener("click", function () {
            const chosen = selectedSource();
            if (chosen) {
                state.daemonOpen[chosen.id] = true;
                saveFolds();
            }
        });
        return link;
    }

    function backendControls(source) {
        const nodes = [modeSwitch()];
        if (state.form.mode === "trace") {
            nodes.push(field("Trace ID", traceInput()));
            nodes.push(el("p", {
                class: "field-note",
                text: "An ID resolves to exactly one trace, so neither the window nor the trace cap applies."
            }));
            return nodes;
        }

        nodes.push(field("Service name", serviceInput()));
        nodes.push(field("Time range", rangeControl(source), state.form.rangeMode === "absolute"
            ? "absolute, fixed at submission"
            : "relative to the moment the run starts"));
        nodes.push(maxTracesBlock());
        return nodes;
    }

    function modeSwitch() {
        const group = el("div", {class: "segmented", role: "radiogroup", "aria-label": "Selection mode"});
        [["service", "Service"], ["trace", "Trace ID"]].forEach(function (entry) {
            const button = el("button", {
                type: "button",
                role: "radio",
                "aria-checked": state.form.mode === entry[0] ? "true" : "false",
                text: entry[1]
            });
            button.addEventListener("click", function () {
                setMode(entry[0]);
            });
            group.appendChild(button);
        });
        return field("Select traces by", group, "one or the other, never both");
    }

    function serviceInput() {
        const input = el("input", {
            type: "text",
            class: "input",
            value: state.form.service,
            placeholder: "order-service",
            spellcheck: "false"
        });
        input.addEventListener("input", function () {
            state.form.service = input.value;
            updateSubmit();
        });
        return input;
    }

    function traceInput() {
        const input = el("input", {
            type: "text",
            class: "input",
            value: state.form.traceId,
            placeholder: "4bf92f3577b34da6a3ce929d0e0e4736",
            spellcheck: "false"
        });
        input.addEventListener("input", function () {
            state.form.traceId = input.value;
            updateSubmit();
        });
        return input;
    }

    function field(label, control, gloss) {
        const heading = el("span", {class: "field-label"}, [el("span", {text: label})]);
        if (gloss) heading.appendChild(el("span", {class: "field-gloss", text: gloss}));
        return el("div", {class: "field"}, [heading, control]);
    }

    function windowLabel() {
        if (state.form.rangeMode === "absolute") {
            return PSL.dtHuman(state.form.fromMs) + " → " + PSL.dtHuman(state.form.toMs);
        }
        return "Last " + PSL.humanDur(state.form.lookback);
    }

    /** The span, and the argument the run will actually carry. */
    function rangeWire() {
        const span = PSL.dur(windowSpanMs());
        return state.form.rangeMode === "absolute"
            ? span + " · from_ms/to_ms"
            : span + " · lookback = " + state.form.lookback;
    }

    function windowSpanMs() {
        return state.form.rangeMode === "absolute"
            ? state.form.toMs - state.form.fromMs
            : PSL.parseDur(state.form.lookback);
    }

    function rangeControl(source) {
        const button = el("button", {
            type: "button",
            class: "range-pill",
            "aria-expanded": String(state.form.pickerOpen)
        }, [
            svg([["circle", {cx: "12", cy: "12", r: "9"}], ["path", {d: "M12 7v5l3.2 2"}]], 14),
            el("span", {class: "range-pill-label", text: windowLabel()}),
            svg([["path", {d: "M6 9l6 6 6-6"}]], 11)
        ]);
        button.addEventListener("click", function () {
            state.form.pickerOpen = !state.form.pickerOpen;
            render();
        });

        const wrap = el("div", {class: "range"}, [
            el("div", {class: "range-row"}, [
                button,
                el("span", {class: "range-wire", text: rangeWire()})
            ])
        ]);
        if (state.form.pickerOpen) wrap.appendChild(rangePicker());
        const notes = rangeConsequences(source);
        if (notes.length > 0) wrap.appendChild(el("div", {class: "consequences"}, notes));
        return wrap;
    }

    /** Consequences appear under the control, not after the run. */
    function rangeConsequences(source) {
        const notes = [];
        const spanMs = windowSpanMs();
        if (spanMs > 86400000) {
            notes.push(consequence("A wider window returns no more data. The run still stops at the trace "
                + "cap, so the result is a sample spread over the period rather than the period itself."));
        }
        if (spanMs > 7 * 86400000) {
            notes.push(consequence("The whole scan has to finish inside the "
                + (state.status.limits.analysis_timeout_seconds) + "-second ceiling, which is usually the "
                + "limit met first. Expect a timeout rather than a result."));
        }
        if (source.retention_hours != null && spanMs > source.retention_hours * 3600000) {
            notes.push(consequence("This source declares it keeps " + PSL.dur(source.retention_hours * 3600000)
                + " of traces. A window beyond that comes back short, or is refused as "
                + "source_rejected_request.", "warn"));
        } else if (source.retention_hours == null && spanMs > 86400000) {
            notes.push(consequence("Nobody declared how far back this source keeps traces, so the Hub "
                + "cannot tell whether it can answer this window at all."));
        }
        return notes;
    }

    function consequence(text, tone) {
        return el("span", {class: "consequence", "data-tone": tone || "muted"}, [
            el("span", {class: "consequence-dot"}),
            el("span", {text: text})
        ]);
    }

    function rangePicker() {
        const backdrop = el("div", {class: "picker-backdrop"});
        backdrop.addEventListener("click", function () {
            state.form.pickerOpen = false;
            render();
        });

        const from = el("input", {type: "datetime-local", class: "input-date", value: PSL.dtLocal(state.form.fromMs)});
        const to = el("input", {type: "datetime-local", class: "input-date", value: PSL.dtLocal(state.form.toMs)});
        const note = el("span", {class: "picker-note"});
        const apply = el("button", {type: "button", class: "picker-apply", text: "Apply range"});

        function readAbsolute() {
            const start = Date.parse(from.value);
            const end = Date.parse(to.value);
            const ordered = Number.isFinite(start) && Number.isFinite(end) && start < end;
            const past = Number.isFinite(end) && end <= Date.now();
            const valid = ordered && past;
            note.textContent = !ordered
                ? "The start must come before the end."
                : !past
                    ? "The end cannot be in the future."
                    : PSL.dur(end - start) + " selected";
            note.setAttribute("data-invalid", valid ? "false" : "true");
            apply.disabled = !valid;
            return {start: start, end: end, valid: valid};
        }

        from.addEventListener("input", readAbsolute);
        to.addEventListener("input", readAbsolute);
        apply.addEventListener("click", function () {
            const read = readAbsolute();
            if (!read.valid) return;
            applyRange("absolute", {fromMs: read.start, toMs: read.end});
        });

        const left = el("div", {class: "picker-pane"}, [
            dateField("From", from),
            dateField("To", to),
            el("div", {class: "picker-apply-row"}, [apply, note]),
            el("div", {class: "picker-rule"}),
            el("p", {class: "overline", text: "Custom relative"}),
            customRelativeRow()
        ]);

        const right = el("div", {class: "picker-pane picker-right"}, [
            el("p", {class: "overline picker-quick-head", text: "Quick ranges"}),
            el("div", {class: "picker-quick"}, QUICK_RANGES.map(function (value) {
                const active = state.form.rangeMode === "relative" && state.form.lookback === value;
                const button = el("button", {
                    type: "button",
                    class: "picker-quick-item",
                    "aria-current": active ? "true" : null,
                    text: "Last " + PSL.humanDur(value)
                });
                button.addEventListener("click", function () {
                    applyRange("relative", {lookback: value});
                });
                return button;
            }))
        ]);

        readAbsolute();
        return el("div", {}, [backdrop, el("div", {class: "picker"}, [left, right])]);
    }

    function applyRange(mode, values) {
        state.form.rangeMode = mode;
        Object.keys(values).forEach(function (key) {
            state.form[key] = values[key];
        });
        state.form.pickerOpen = false;
        render();
    }

    function dateField(label, control) {
        return el("label", {class: "picker-field"}, [
            el("span", {class: "picker-field-label", text: label}),
            control
        ]);
    }

    function customRelativeRow() {
        const qty = el("input", {
            type: "number",
            class: "input-qty",
            min: "1",
            value: String(state.form.customQty)
        });
        const units = el("div", {class: "segmented segmented-sm", role: "radiogroup", "aria-label": "Unit"});
        ["m", "h", "d"].forEach(function (unit) {
            const button = el("button", {
                type: "button",
                role: "radio",
                "aria-checked": state.form.customUnit === unit ? "true" : "false",
                text: unit
            });
            // Picking a unit selects it. Applying is a separate, deliberate click,
            // so a half-typed quantity is never submitted by choosing a unit.
            button.addEventListener("click", function () {
                state.form.customUnit = unit;
                state.form.customQty = Math.max(1, Number(qty.value) || 1);
                render();
            });
            units.appendChild(button);
        });

        const apply = el("button", {type: "button", class: "pill-button pill-sm", text: "Apply"});
        apply.addEventListener("click", function () {
            const quantity = Math.max(1, Number(qty.value) || 1);
            state.form.customQty = quantity;
            applyRange("relative", {lookback: quantity + state.form.customUnit});
        });

        return el("div", {class: "picker-custom"}, [
            el("span", {class: "picker-custom-lead", text: "Last"}),
            qty,
            units,
            apply
        ]);
    }

    function maxTracesBlock() {
        const cap = state.status.limits.max_traces_cap;
        const band = PSL.weightBand(state.form.maxTraces, cap);
        const over = band.key === "over" || band.key === "invalid";

        const number = el("input", {
            type: "number",
            id: "traces-number",
            class: "input input-traces",
            min: "1",
            max: String(cap),
            value: String(state.form.maxTraces)
        });
        if (over) number.setAttribute("data-over", "true");
        number.setAttribute("data-band", band.key);
        number.addEventListener("input", function () {
            setMaxTraces(Number(number.value));
        });

        const head = el("div", {class: "traces-head"}, [
            number,
            el("span", {id: "traces-band", class: "band-chip", style: bandStyle(band), text: band.label}),
            el("span", {
                id: "traces-cap",
                class: "traces-cap",
                "data-over": over ? "true" : "false",
                text: capNote(band, cap)
            })
        ]);

        const slider = el("input", {
            type: "range",
            id: "traces-slider",
            min: "1",
            max: String(cap),
            step: "1",
            value: String(Math.min(Math.max(state.form.maxTraces, 1), cap)),
            "aria-label": "Max traces"
        });
        slider.addEventListener("input", function () {
            setMaxTraces(Number(slider.value));
        });

        // The container carries the pill radius and clips its children, so only the
        // outer ends are rounded and the two inner joins stay square.
        const segments = el("div", {class: "band-segs", "aria-hidden": "true"},
            bands(cap).map(function (band) {
                const segment = el("span", {class: "band-seg", "data-seg": band.tone});
                segment.style.width = band.width;
                return segment;
            }));

        const block = el("div", {class: "field"}, [
            // A div and not a label: the "?" is a button, which a label may not
            // contain, and the real binding is the `for` on the name beside it.
            el("div", {class: "field-label"}, [
                el("label", {for: "traces-number", text: "Max traces"}),
                helpDot("This and the window travel to the backend in one search, the window as its "
                    + "time bounds, this as its limit. It is a ceiling and not a target, so a window "
                    + "holding fewer traces returns fewer. When it holds more, the backend picks which "
                    + "ones and they are not an even spread, so narrow the window when you need a given "
                    + "stretch covered. Traces come back whole rather than sampled, so this number bounds "
                    + "what the run costs."),
                el("span", {class: "field-gloss", text: "how much comes back, not how far back"})
            ]),
            head,
            el("div", {class: "band-track"}, [segments, slider]),
            bandScale(cap),
            el("p", {id: "traces-body", class: "band-body", style: "color:" + band.fg, text: band.body}),
            // A slot rather than a conditional child: the acknowledgement appears and
            // disappears as the count crosses the ceiling, and refreshing it in place
            // keeps the rest of the block untouched.
            el("div", {id: "traces-ack"}, band.needsAck ? [heavyAck()] : [])
        ]);
        block.appendChild(sinkPanel());
        // A slot rather than a conditional child, like traces-ack above: the
        // deferred runs fetch fills it in place without touching the form.
        const slot = el("div", {id: "weight-history"});
        const history = weightHistory();
        if (history) slot.appendChild(history);
        block.appendChild(slot);
        return block;
    }

    /** The sink-panel scaffold both info blocks share: head, subtitle, rows. */
    function sinkBlock(head, sub, rows, fold) {
        const body = el("dl", {class: "sink-rows"}, rows.flatMap(function (row) {
            return [el("dt", {text: row[0]}), el("dd", {text: row[1]})];
        }));
        if (!fold) {
            return el("div", {class: "sink"}, [
                el("div", {class: "sink-head"}, [
                    el("span", {class: "overline", text: head}),
                    el("span", {class: "sink-sub", text: sub})
                ]),
                body
            ]);
        }

        // The same fold as the daemon row's terminal block, down to the chevron:
        // the heading and its subtitle stay put, only the rows go.
        const toggle = el("button", {
            type: "button",
            class: "sink-more",
            "aria-expanded": fold.open ? "true" : "false"
        }, [el("span", {class: "overline", text: head})]);
        body.hidden = !fold.open;
        const root = el("div", {class: "sink"}, [
            el("div", {class: "sink-head"}, [toggle, el("span", {class: "sink-sub", text: sub})]),
            body
        ]);
        // Marked rather than inferred from the hidden body: the heading's own
        // bottom margin has nothing under it when folded, and .sink-head is shared
        // with three blocks that do want it.
        root.toggleAttribute("data-folded", !fold.open);
        toggle.addEventListener("click", function () {
            const next = toggle.getAttribute("aria-expanded") !== "true";
            toggle.setAttribute("aria-expanded", next ? "true" : "false");
            body.hidden = !next;
            root.toggleAttribute("data-folded", !next);
            fold.onToggle(next);
        });
        return root;
    }

    /**
     * What this source's own runs weighed, at the count they asked for. A
     * measurement rather than a model: a report's size follows how many spans
     * its traces carry, which differs per service and which the launcher cannot
     * know before the run. Absent until this source has a measured run.
     */
    function weightHistory() {
        const source = selectedSource();
        if (!source || !state.runs) return null;
        const past = state.runs.filter(function (run) {
            const result = run.result || {};
            // Trace-id runs are excluded: their weight says nothing about the
            // slider. A missing max_traces on a service run is the server-side
            // default, not an unmeasured one.
            return run.source_id === source.id &&
                run.status === "succeeded" &&
                typeof result.report_bytes === "number" &&
                (run.request || {}).trace_id === undefined;
        }).slice(0, 3);
        if (past.length === 0) return null;

        return sinkBlock("// what your own runs weighed", "Measured on this source, not predicted.",
            past.map(function (run) {
                const traces = typeof run.request.max_traces === "number" ? run.request.max_traces : 100;
                return [PSL.bytes(run.result.report_bytes),
                    traces + " traces, " + run.result.findings + " findings, "
                    + PSL.dur(Date.now() - (run.finished_at_ms || run.created_at_ms)) + " ago"];
            }));
    }

    function capNote(band, cap) {
        if (band.key === "invalid") return "at least 1";
        if (band.key === "over") return "above the hard cap of " + cap + ", the service will reject this";
        return "hard cap " + cap;
    }

    /**
     * The bands that exist at this cap, each with the boundary it ends at and
     * its share of the rule. A cap at or below a boundary means that band never
     * occurs: it is dropped rather than drawn at zero width under a label
     * repeating the cap.
     */
    function bands(cap) {
        const inner = [["ok", 500, "safe"], ["warn", 1200, "heavy"]]
            .filter(function (band) {
                return band[1] < cap;
            });
        // The stripe that ends at the cap takes the tone of whichever band the cap
        // itself falls in, which is the one after the last inner boundary kept.
        // Always painting it crit would turn a Hub capped at 500 into an entirely
        // red rule over a range that is all comfortable.
        const TONES = ["ok", "warn", "crit"];
        const kept = inner.concat([[TONES[inner.length], cap, "cap"]]);

        let previous = 0;
        return kept.map(function (band) {
            const share = (band[1] - previous) / cap;
            previous = band[1];
            return {
                tone: band[0],
                label: group(band[1]) + " " + band[2],
                width: (share * 100).toFixed(2) + "%"
            };
        });
    }

    /** Each label sits at the right edge of its own band, so it marks a boundary. */
    function bandScale(cap) {
        const current = bands(cap);
        const grid = el("div", {class: "band-scale-grid"}, current.map(function (band) {
            return el("span", {text: band.label});
        }));
        grid.style.gridTemplateColumns = current.map(function (band) {
            return band.width;
        }).join(" ");
        return el("div", {class: "band-scale"}, [
            el("span", {class: "band-scale-start", text: "1"}),
            grid
        ]);
    }

    function group(value) {
        // Thousands separators on the integer part only: a decimal such as
        // 0.6667 must not come out as 0.6 667.
        const text = String(value);
        const point = text.indexOf(".");
        const whole = point < 0 ? text : text.slice(0, point);
        return whole.replace(/\B(?=(\d{3})+(?!\d))/g, "\u202f") + (point < 0 ? "" : text.slice(point));
    }

    // render() returns early when the status is missing, so every caller runs
    // with one in hand.
    function tracesCap() {
        return state.status.limits.max_traces_cap;
    }

    function embeddedCap() {
        return state.status.limits.max_traces_embedded;
    }

    /**
     * What the sink guarantees, measured rather than predicted. The design bans
     * predicting a byte size, and its reason still holds: SQL template lengths
     * move a fixed-count report by tens of kilobytes. Both ends of the range are
     * fixed, though, so they can be stated as facts.
     */
    function sinkPanel() {
        const rows = [
            ["550 KB", "The floor. Fonts, styles and the dashboard itself, present in every report "
            + "whether it found one problem or none."],
            ["every finding", "Every run this Hub executes renders every finding it found, at any "
            + "size. The count on the dashboard is the count that was found."],
            [String(embeddedCap()) + " trees", "Span trees embedded, for the findings with the highest "
            + "aggregate impact. The rest open without a tree and say so, with the trace id to read "
            + "that one on its own, which the Trace ID mode above runs directly. Set by whoever "
            + "operates this Hub."],
            ["25", "Hard cap on the top offenders embedded for the Carbon tab, whatever the run size. "
            + "The full ranking is still computed, only the embed is capped."],
            ["no ceiling", "The file has no size target of its own once every finding is kept. A run "
            + "that finds a great deal produces a report that takes a moment to open."]
        ];
        // Folded until the reader opens it, like every other fold in the product,
        // and remembered from then on.
        return sinkBlock("// what comes back, and what it caps",
            "From the sink and this Hub's settings, not predictions.", rows, {
                open: state.panelOpen.caps === true,
                onToggle: function (open) {
                    state.panelOpen.caps = open;
                    saveFolds();
                }
            });
    }

    function bandStyle(band) {
        return "color:" + band.fg + ";background:" + band.bg;
    }

    function heavyAck() {
        const node = checkbox(
            state.form.ackHeavy,
            "I accept a long run and a heavy report.",
            function (checked) {
                state.form.ackHeavy = checked;
                updateSubmit();
            });
        node.classList.add("checkbox-pill");
        return node;
    }

    /**
     * A producer behind the engine is worth saying out loud: perf-sentinel is
     * pre-1.0, so a detector added between minors does not run on the older
     * binary at all, and its absence looks exactly like a clean service.
     */
    function skewNotice(source, skew) {
        const engine = state.status.engine_version;
        const behind = skew.dir === "behind";
        return el("section", {class: "notice-block", "data-tone": behind ? "warn" : "info"}, [
            warningGlyph(16),
            el("div", {class: "notice-block-text"}, [
                el("p", {
                    class: "notice-block-title",
                    text: source.name + " runs " + source.producer_version + ", " + skew.label + " the "
                        + engine + " binary embedded in the Hub."
                }),
                el("p", {
                    class: "notice-block-body",
                    text: behind
                        ? "perf-sentinel is pre-1.0, so detectors change between minors. A detector added in "
                        + engine + " does not run on this producer at all, and its absence looks exactly like "
                        + "a clean service. Read a low finding count from this source as unmeasured, not as healthy."
                        : "Envelopes are additive, so nothing breaks. Findings from a detector this Hub does not "
                        + "know about arrive unnamed. The Hub compares two version strings and cannot know "
                        + "whether this minor changed detection at all."
                }),
                behind ? upgradeLine(source, engine) : null
            ])
        ]);
    }

    /**
     * Where the newer version comes from, which is not the same place for every
     * source. A daemon is upgraded through its chart, so pointing its operator at
     * a binary would be pointing at the wrong artefact.
     */
    function upgradeLine(source, engine) {
        const line = el("p", {class: "notice-block-body"}, [
            el("span", {text: "Get " + engine + ": "}),
            el("a", {
                class: "notice-block-link",
                href: PSL.releaseUrl(engine),
                target: "_blank",
                rel: "noopener noreferrer",
                text: "release notes and binaries"
            })
        ]);
        if (source.kind !== "daemon") return line;
        line.appendChild(el("span", {text: ", or the chart this daemon is deployed from, "}));
        line.appendChild(el("a", {
            class: "notice-block-link",
            href: PSL.CHART_PAGE,
            target: "_blank",
            rel: "noopener noreferrer",
            text: "every chart version and the engine it ships"
        }));
        line.appendChild(el("span", {text: ". The chart itself is "}));
        // A coordinate for helm, not a link: the scheme is not one a browser opens.
        line.appendChild(el("code", {class: "code-inline", text: PSL.CHART_COORDINATE}));
        line.appendChild(el("span", {text: "."}));
        return line;
    }

    function unreachableNotice(source) {
        const text = el("div", {class: "notice-block-text"}, [
            el("p", {
                class: "notice-block-title",
                text: source.name + " has been unreachable for " + PSL.dur(Date.now() - source.unreachable_since_ms) + "."
            })
        ]);
        if (source.last_success_ms) {
            text.appendChild(el("p", {
                class: "notice-block-body",
                text: "Last successful contact " + PSL.dur(Date.now() - source.last_success_ms) + " ago."
            }));
        }
        if (source.last_error_code) {
            text.appendChild(el("p", {class: "notice-block-body"}, [
                el("span", {text: "The last attempt returned "}),
                el("span", {class: "code-inline", text: source.last_error_code}),
                el("span", {text: ": " + (PSL.ERRORS[source.last_error_code] || "the Hub could not reach it.")})
            ]));
        }
        text.appendChild(el("p", {
            class: "notice-block-body",
            text: "Running now will consume a worker slot and will almost certainly end with the same code."
        }));
        text.appendChild(checkbox(
            state.form.ackUnreachable,
            "Run it anyway",
            function (checked) {
                state.form.ackUnreachable = checked;
                updateSubmit();
            }));

        return el("section", {class: "notice-block", "data-tone": "warn"}, [warningGlyph(17), text]);
    }

    /** The circle-exclamation every crit banner carries, drawn once. */
    function critGlyph(size) {
        return svg([
            ["circle", {cx: "12", cy: "12", r: "9"}],
            ["path", {d: "M12 7.5v5M12 15.8v.2"}]
        ], size);
    }

    function warningGlyph(size) {
        return svg([
            ["path", {d: "M10.3 3.9 1.9 18a2 2 0 0 0 1.7 3h16.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"}],
            ["path", {d: "M12 9v4M12 17h.01"}]
        ], size);
    }

    function checkbox(checked, label, onChange) {
        const input = el("input", {type: "checkbox"});
        input.checked = checked;
        input.addEventListener("change", function () {
            onChange(input.checked);
        });
        return el("label", {class: "checkbox"}, [input, el("span", {text: label})]);
    }

    /** Reported by the service, not assumed: the button is a promise of cost. */
    function costBand() {
        const limits = state.status.limits;
        const queue = state.status.queue_depth;
        const cells = [
            [String(limits.max_traces_cap), "traces", "Hard cap per run", "The service rejects anything above it."],
            [String(limits.analysis_timeout_seconds), "s", "Timeout", "Then the run is killed, marked timeout."],
            [String(state.status.workers), "workers",
                queue === 1 ? "1 job queued now" : queue + " jobs queued now",
                "That many runs at a time across the whole Hub."],
            [String(limits.report_retention_hours), "h", "Report retention", "Then the file is deleted. Links die."]
        ];
        return el("section", {class: "card cost"}, [
            el("div", {class: "cost-head"}, [
                el("span", {class: "overline", text: "// what this run costs"}),
                el("span", {class: "cost-sub", text: "Reported by the service, not assumed."})
            ]),
            el("div", {class: "cost-grid"}, cells.map(function (cell) {
                return el("div", {class: "cost-cell"}, [
                    el("p", {class: "cost-figure"}, [
                        el("span", {text: cell[0]}),
                        el("span", {class: "cost-unit", text: cell[1]})
                    ]),
                    el("p", {class: "cost-label", text: cell[2]}),
                    el("p", {class: "cost-note", text: cell[3]})
                ]);
            }))
        ]);
    }

    /**
     * What blocks the run, or null when nothing does. Mirrors the server's own
     * rules so the operator is told before spending a round trip.
     */
    function submitBlocker() {
        const source = selectedSource();
        if (!source) return "Pick a source.";
        if (!state.status.engine_version) return "This Hub has no analysis engine configured.";
        if (!source.reachable && !state.form.ackUnreachable) return "Confirm you want to run against an unreachable source.";
        if (source.kind === "daemon") return null;
        if (state.form.mode === "trace") {
            return state.form.traceId.trim() ? null : "Enter a trace ID.";
        }
        if (!state.form.service.trim()) return "Enter a service name.";
        const band = PSL.weightBand(state.form.maxTraces, tracesCap());
        if (band.key === "over") return "The trace cap is above what the service accepts.";
        if (band.key === "invalid") return "A run needs at least one trace.";
        return band.needsAck && !state.form.ackHeavy ? "Confirm the long run and heavy report." : null;
    }

    /** Restates the request in a sentence, so the button is not a leap of faith. */
    function submitSentence() {
        const source = selectedSource();
        if (!source) return "";
        if (source.kind === "daemon") {
            return "Takes a snapshot of what " + source.name + " holds in memory. No query is sent to a "
                + "trace backend. " + queuePhrase();
        }
        if (state.form.mode === "trace") {
            return "Fetches one trace by ID from " + source.name + ". " + queuePhrase();
        }
        return "Reads up to " + state.form.maxTraces + " traces for "
            + (state.form.service.trim() || "a service") + " across "
            + (state.form.rangeMode === "absolute" ? "the selected window" : "the last " + PSL.humanDur(state.form.lookback))
            + " of " + source.name + ". " + queuePhrase();
    }

    function queuePhrase() {
        const queue = state.status.queue_depth;
        if (queue === 0) return "Nothing is queued ahead of it.";
        return queue === 1 ? "Queued behind 1 job." : "Queued behind " + queue + " jobs.";
    }

    function submitRow() {
        const button = el("button", {type: "button", class: "submit", id: "submit"}, [
            playGlyph(),
            el("span", {text: "Run analysis"})
        ]);
        button.addEventListener("click", submit);
        const row = el("div", {class: "submit-row"}, [
            button,
            el("p", {class: "submit-sentence", id: "submit-sentence"})
        ]);
        queueMicrotask(updateSubmit);
        return row;
    }

    function playGlyph() {
        const node = document.createElementNS("http://www.w3.org/2000/svg", "svg");
        node.setAttribute("viewBox", "0 0 24 24");
        node.setAttribute("width", "15");
        node.setAttribute("height", "15");
        node.setAttribute("fill", "currentColor");
        node.setAttribute("aria-hidden", "true");
        const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
        path.setAttribute("d", "M7 4.5v15l13-7.5z");
        node.appendChild(path);
        return node;
    }

    function updateSubmit() {
        const button = document.getElementById("submit");
        const sentence = document.getElementById("submit-sentence");
        if (!button || !sentence) return;
        const blocker = submitBlocker();
        button.disabled = blocker !== null;
        button.title = blocker || "";
        sentence.textContent = blocker || submitSentence();
        sentence.setAttribute("data-blocked", blocker ? "true" : "false");
        refreshTerminal();
    }

    function buildRequest(source) {
        if (source.kind === "daemon") return {};
        if (state.form.mode === "trace") {
            const trace = {trace_id: state.form.traceId.trim()};
            if (Object.keys(state.form.detection).length > 0) trace.detection = state.form.detection;
            return trace;
        }
        const request = {service: state.form.service.trim(), max_traces: state.form.maxTraces};
        if (Object.keys(state.form.detection).length > 0) request.detection = state.form.detection;
        if (state.form.rangeMode === "absolute") {
            request.from_ms = state.form.fromMs;
            request.to_ms = state.form.toMs;
        } else {
            request.lookback = state.form.lookback;
        }
        return request;
    }

    function submit() {
        const source = selectedSource();
        if (!source || submitBlocker()) return;
        const button = document.getElementById("submit");
        button.disabled = true;

        fetch("/api/analyses", {
            method: "POST",
            headers: {"content-type": "application/json"},
            body: JSON.stringify({source_id: source.id, request: buildRequest(source)})
        }).then(function (response) {
            return response.json().then(function (payload) {
                return {ok: response.ok, payload: payload};
            });
        }).then(function (result) {
            if (!result.ok) throw new Error(result.payload.detail || "The Hub refused the request.");
            location.hash = "#/run/" + result.payload.id;
        }).catch(function (error) {
            const sentence = document.getElementById("submit-sentence");
            if (sentence) {
                sentence.textContent = String(error.message || error);
                sentence.setAttribute("data-blocked", "true");
            }
            updateSubmit();
        });
    }


    // ------------------------------------------------------- screen: one run

    function renderRunScreen(id) {
        const run = state.run;
        const section = el("section", {}, [backLink()]);
        if (state.runError) {
            section.appendChild(el("div", {class: "empty-state", text: "No analysis with that ID."}));
            return section;
        }
        if (!run || run.id !== id) {
            section.appendChild(el("div", {class: "card skeleton", style: "height:220px;margin-top:16px"}));
            return section;
        }

        const key = PSL.statusKey(run);
        const view = runView(run, key);
        section.appendChild(el("div", {class: "run-head"}, [
            el("span", {class: "status-pill", "data-status": key, text: key}),
            el("span", {class: "run-id", text: run.id})
        ]));
        section.appendChild(el("h1", {class: "page-title", text: view.headline}));
        const sub = typeof view.sub === "function" ? view.sub : null;
        section.appendChild(live(el("p", {class: "page-sub", text: sub ? sub() : view.sub}), sub));

        const left = el("div", {class: "run-left"}, [eventLog(run, key)]);
        const outcome = outcomePanel(run, key, view);
        if (outcome) left.appendChild(outcome);
        section.appendChild(el("div", {class: "run-grid"}, [left, factsRail(run, key)]));
        return section;
    }

    function backLink() {
        return el("a", {class: "back-pill", href: "#/recent"}, [
            svg([["path", {d: "M15 18l-6-6 6-6"}]], 13),
            el("span", {text: "All analyses"})
        ]);
    }

    /** Headline and sub-line per state, in the source's own terms. */
    function runView(run, key) {
        if (key === "queued") {
            return {
                headline: "Waiting for a worker.",
                sub: "Every worker is busy. Nothing has been read from " + run.source_name
                    + " yet, so nothing has been spent."
            };
        }
        if (key === "running") {
            return {
                headline: "Reading " + run.source_name + ".",
                sub: "A worker holds this job. The next thing that happens is a result or a failure, with "
                    + "nothing in between."
            };
        }
        if (key === "empty") {
            return {
                headline: "It succeeded, and there is nothing in it.",
                sub: "This is not a failure and not an error. The source answered correctly, and the answer "
                    + "was zero traces."
            };
        }
        if (key === "succeeded") {
            const result = run.result || {};
            const caveats = (result.warnings || []).length;
            return {
                headline: caveats > 0
                    ? result.findings + " findings, and " + (caveats === 1 ? "a caveat" : caveats + " caveats")
                    + " you should read first."
                    : result.findings + " findings.",
                sub: function () {
                    return "The report is ready and will be deleted in "
                        + PSL.durPrecise(run.expires_at_ms - Date.now()) + ".";
                }
            };
        }
        if (key === "interrupted") {
            return {
                headline: "The service restarted while this was running.",
                sub: "It stopped after " + PSL.dur((run.finished_at_ms || 0) - (run.started_at_ms || run.created_at_ms))
                    + " of work. This is a resumption, not an error to investigate."
            };
        }
        if (key === "expired") {
            return {
                headline: "This report was deleted.",
                sub: function () {
                    return "Reports live " + state.status.limits.report_retention_hours + " hours. This one expired "
                        + PSL.durPrecise(Date.now() - run.expires_at_ms) + " ago and the file is gone.";
                }
            };
        }
        return {
            headline: "Failed: " + String(run.error_code || "internal").replace(/_/g, " ") + ".",
            sub: "The Hub does not expose the process's error output by design. It gives one code out of "
                + "eight, and this is what that code means."
        };
    }

    /**
     * A receipt, not a feed. Every line is a timestamp the Hub wrote: the design
     * calls for a `dequeued` line too, but this service records one instant for
     * dequeue and start, and inventing a second would be interpolation.
     */
    function eventLog(run, key) {
        const rows = [logRow(run.created_at_ms, "accepted", "the request was validated and queued", "muted")];
        if (run.started_at_ms) {
            rows.push(logRow(run.started_at_ms, "started",
                run.kind === "daemon" ? "reading the daemon's in-memory store" : "reading " + PSL.KIND_LABEL[run.kind],
                "brand"));
        }
        if (key === "running") rows.push(logRow(null, "running", "no further event until the engine returns", "brand"));
        if (key === "queued") rows.push(logRow(null, "waiting", "every worker is busy, nothing has been read yet", "muted"));
        if (key === "succeeded" || key === "empty") {
            rows.push(logRow(run.finished_at_ms, "succeeded",
                "report written, retained " + state.status.limits.report_retention_hours + " h", "ok"));
        }
        if (key === "failed") rows.push(logRow(run.finished_at_ms, "failed", run.error_code, "crit"));
        if (key === "interrupted") {
            rows.push(logRow(run.finished_at_ms, "interrupted",
                "the Hub restarted, the run was abandoned and not replayed", "info"));
        }
        if (key === "expired") {
            rows.push(logRow(run.finished_at_ms, "succeeded", "report written", "muted"));
            rows.push(logRow(run.expires_at_ms, "deleted",
                "retention reached, the report no longer exists", "muted"));
        }

        return el("section", {class: "card log-card", "aria-label": "Service events"}, [
            el("div", {class: "log-head"}, [
                el("span", {class: "overline", text: "// service events"}),
                el("span", {class: "log-head-note", text: "Only what the Hub actually recorded."})
            ]),
            el("div", {class: "log"}, rows),
            el("div", {class: "log-foot"}, [el("p", {text: logClosing(run, key)})])
        ]);
    }

    function logRow(ms, name, detail, tone) {
        return el("div", {class: "log-row"}, [
            el("span", {class: "log-time", text: ms ? PSL.clock(ms) : "…"}),
            el("span", {class: "log-dot", "data-tone": tone}),
            el("span", {class: "log-text"}, [
                el("span", {class: "log-name", "data-tone": tone, text: name}),
                el("span", {class: "log-detail", text: detail})
            ])
        ]);
    }

    function logClosing(run, key) {
        if (key === "running" || key === "queued") {
            return "The engine reports nothing between start and finish. There is no percentage to show "
                + "and no arrival time to predict, so this screen shows neither. Only the events above, the "
                + "time spent, and the ceiling at which the service gives up. Expect one more line, not a stream.";
        }
        const instant = run.finished_at_ms && (run.finished_at_ms - run.created_at_ms) < 10000;
        return instant
            ? "This run was read and finished in one step, so every line above was written at once. It is "
            + "a receipt of what happened, not a feed."
            : "Every line above is a timestamp the Hub wrote. Nothing here is interpolated.";
    }

    function factsRail(run, key) {
        const elapsedMs = key === "queued"
            ? Date.now() - run.created_at_ms
            : (run.finished_at_ms || Date.now()) - (run.started_at_ms || run.created_at_ms);
        const figure = el("div", {class: "elapsed", "data-running": key === "running" ? "true" : "false"});
        PSL.durParts(elapsedMs).forEach(function (part) {
            figure.appendChild(el("span", {class: "elapsed-part"}, [
                el("span", {class: "elapsed-n", text: part.n}),
                el("span", {class: "elapsed-u", text: part.u})
            ]));
        });

        const elapsed = el("section", {class: "card rail-card"}, [
            el("p", {class: "overline", text: "// elapsed"}),
            figure
        ]);
        // The only bar in the product, and only while running: it measures a known
        // ceiling, not progress toward an unknown total.
        if (key === "running") elapsed.appendChild(ceilingRule(elapsedMs));
        elapsed.appendChild(el("p", {class: "rail-note", text: ceilingNote(key, elapsedMs)}));

        return el("aside", {class: "rail"}, [elapsed, requestCard(run)]);
    }

    function ceilingRule(elapsedMs) {
        const ceilingMs = state.status.limits.analysis_timeout_seconds * 1000;
        const fill = el("span", {class: "ceiling-fill"});
        fill.style.width = Math.min(100, (elapsedMs / ceilingMs) * 100).toFixed(1) + "%";
        if (elapsedMs > ceilingMs * 0.8) fill.setAttribute("data-near", "true");
        return el("div", {class: "ceiling", "aria-hidden": "true"}, [fill]);
    }

    function ceilingNote(key, elapsedMs) {
        const seconds = state.status.limits.analysis_timeout_seconds;
        if (key === "queued") {
            return "The " + seconds + "-second ceiling starts when a worker picks the job up, not now.";
        }
        if (key !== "running") return "Total time the run occupied a worker.";
        const left = seconds * 1000 - elapsedMs;
        return left <= 0
            ? "Past the " + seconds + " s ceiling. The run should already have been killed and marked timeout."
            : "Hard stop at " + seconds + " s, then the run is killed and marked timeout. "
            + PSL.dur(left) + " of ceiling left.";
    }

    function requestCard(run) {
        const request = run.request || {};
        const facts = [["source", run.source_name, "ui"], ["type", PSL.KIND_LABEL[run.kind] || run.kind, "mono"]];
        ["service", "trace_id", "lookback", "max_traces"].forEach(function (name) {
            if (request[name] != null) facts.push([name, String(request[name]), "mono"]);
        });
        if (request.from_ms) facts.push(["window", PSL.dtHuman(request.from_ms) + " → " + PSL.dtHuman(request.to_ms), "mono"]);
        Object.keys(request.detection || {}).forEach(function (name) {
            facts.push([name, String(request.detection[name]), "warn"]);
        });
        if (!request.service && !request.trace_id) facts.push(["parameters", "none", "muted"]);
        facts.push(["requested by", run.requested_by, "mono"]);
        facts.push(["detected by", run.producer_version
            ? PSL.detector(run.kind) + " " + run.producer_version
            : "not yet known", PSL.skew(run.producer_version) ? "warn" : "mono"]);
        facts.push(["expires", expiryText(run),
            run.expires_at_ms && run.expires_at_ms < Date.now() ? "crit" : "mono",
            function () {
                return expiryText(run);
            }]);

        return el("section", {class: "request"}, [
            el("p", {class: "overline", text: "// request"}),
            el("div", {class: "request-grid"}, facts.map(function (fact) {
                return el("div", {class: "fact-card"}, [
                    el("span", {class: "fact-card-k", text: fact[0]}),
                    live(el("span", {
                        class: "fact-card-v",
                        "data-tone": fact[2],
                        text: fact[1],
                        title: fact[1]
                    }), fact[3])
                ]);
            }))
        ]);
    }

    function expiryText(run) {
        if (!run.expires_at_ms) return "not until it succeeds";
        const delta = run.expires_at_ms - Date.now();
        return delta > 0 ? "in " + PSL.durPrecise(delta) : PSL.durPrecise(-delta) + " ago";
    }

    function outcomePanel(run, key, _) {
        if (key === "running" || key === "queued") return null;
        const spec = outcomeSpec(run, key);
        const panel = el("section", {class: "outcome", "data-tone": spec.tone}, [
            el("p", {class: "overline", text: "// " + spec.title}),
            el("p", {class: "outcome-body", text: spec.body})
        ]);
        if (spec.counts) panel.appendChild(countStrip(spec.counts, {toned: true, filled: 1}));
        const trimmed = trimNotice(run);
        if (trimmed) panel.appendChild(trimmed);
        (spec.warnings || []).forEach(function (warning) {
            panel.appendChild(el("div", {class: "outcome-warning"}, [
                el("span", {class: "outcome-warning-kind", text: warning.kind}),
                el("span", {class: "outcome-warning-message", text: warning.message})
            ]));
        });
        panel.appendChild(actionRow(run, spec));
        return panel;
    }

    function outcomeSpec(run, key) {
        const result = run.result || {};
        if (key === "succeeded") {
            return {
                // The run succeeding and the gate passing are two different verdicts.
                // A green panel announcing a failed gate contradicts its own sentence.
                tone: result.quality_gate_passed ? "ok" : "crit", title: "result",
                body: result.quality_gate_passed
                    ? "The quality gate passed. The dashboard holds the full detail."
                    : "The quality gate did not pass. The dashboard holds the full detail.",
                counts: [
                    [result.quality_gate_passed ? "PASS" : "FAIL", "quality gate",
                        result.quality_gate_passed ? "ok" : "crit"],
                    [String(result.findings), result.kept_findings == null ? "findings" : "found",
                        result.critical > 0 ? "crit" : result.warning > 0 ? "warn"
                            : result.info > 0 ? "info" : "ok"],
                    [String(result.critical), "critical", "crit"],
                    [String(result.warning), "warning", "warn"],
                    [String(result.info), "info", "info"],
                    [String(result.traces_analyzed), "traces read", "text"]
                ],
                warnings: result.warnings,
                primary: {label: "Open the dashboard", href: "#/report/" + run.id, filled: true},
                note: function () {
                    return "Opens on this origin. The link dies in "
                        + PSL.durPrecise(run.expires_at_ms - Date.now()) + ".";
                }
            };
        }
        if (key === "empty") {
            return {
                tone: "warn", title: "empty result",
                body: run.source_name + " had nothing for the engine to analyse. The report exists, and it is "
                    + "blank. Opening it will show an empty dashboard. That is the expected outcome, not a "
                    + "rendering fault.",
                counts: [
                    [result.quality_gate_passed ? "PASS" : "FAIL", "quality gate", "muted"],
                    [String(result.findings), "findings", "warn"],
                    [String(result.traces_analyzed), "traces read", "warn"]
                ],
                warnings: result.warnings,
                primary: {label: "Wait and run it again", href: "#/new", filled: false},
                secondary: {label: "Open the blank dashboard anyway", href: "#/report/" + run.id},
                note: "A quality gate that passes on zero traces has not measured anything."
            };
        }
        if (key === "failed") {
            return {
                tone: "crit", title: run.error_code || "internal",
                body: run.source_name + ": " + (PSL.ERRORS[run.error_code] || "it failed for an unnamed reason."),
                primary: {label: "Run it again", href: "#/new", filled: false},
                secondary: {label: "Check the source", href: "#/sources"},
                note: "Nothing was stored, so nothing expires."
            };
        }
        if (key === "interrupted") {
            return {
                tone: "info", title: "resume",
                body: "The Hub never replays an interrupted run on its own. A silent retry could fire a second "
                    + "heavy query at " + run.source_name + " without anyone asking for it, so the decision stays "
                    + "yours. The parameters are unchanged and ready to send again.",
                primary: {
                    label: "Resume with the same parameters",
                    action: function (button) {
                        resubmit(run, button);
                    },
                    filled: true
                },
                note: PSL.argsLine(run)
            };
        }
        return {
            tone: "muted", title: "expired",
            body: "Retention is not configurable from here. Running the same analysis again produces a new "
                + "report with a new clock. It will not reproduce the old one, because the source has moved "
                + "on since then.",
            primary: {label: "Run it again", href: "#/new", filled: false},
            note: PSL.argsLine(run)
        };
    }

    /**
     * The sink drops findings to fit its budget, and the count on the card is
     * what the engine found, not what the report holds. Said above the link,
     * because it changes how the numbers should be read.
     */
    function trimNotice(run) {
        const result = run.result || {};
        if (result.kept_findings == null || result.kept_findings >= result.findings) return null;
        return el("div", {class: "outcome-warning"}, [
            el("span", {class: "outcome-warning-kind", text: "trimmed"}),
            el("span", {
                class: "outcome-warning-message",
                text: result.findings + " findings were found and " + result.kept_findings
                    + " are in the report. The sink dropped the rest to fit, critical last, so what "
                    + "survived is what mattered most."
            })
        ]);
    }

    /**
     * A cell is [figure, label, tone, move]. `move` stays a number throughout, so
     * the fill is named by index in the options rather than by a fifth slot that
     * would sit where a facts tuple keeps something else entirely.
     *
     * `options.toned` carries the tone onto the cell, the way the rendered
     * dashboard draws the same figures: `options.filled` names the one cell shown
     * as a solid block of its tone, the rest take their own pastel. The daemon
     * gauges pass no options and keep a neutral strip, because their tone means
     * "near a cap", which is not a severity.
     */
    function countStrip(counts, options) {
        const opts = options || {};
        return el("div", {class: "counts"}, counts.map(function (cell, index) {
            const tone = opts.toned && cell[2] && cell[2] !== "text" ? cell[2] : null;
            const filled = index === opts.filled;
            const figure = el("span", {class: "count-n", "data-tone": cell[2]},
                [document.createTextNode(cell[0])]);
            if (typeof cell[3] === "number") figure.appendChild(moveBadge(cell[3]));
            return el("div", {
                class: "count",
                "data-grad": tone && !filled ? tone : null,
                "data-kpi": tone && filled ? tone : null
            }, [figure, el("span", {class: "count-l", text: cell[1]})]);
        }));
    }

    /**
     * How far a gauge moved since the last read. Up is the bad direction here:
     * every one of these counts toward a cap, so a rise is ground lost and a
     * fall is ground won, which is the opposite of the usual reading.
     */
    function moveBadge(move) {
        const badge = el("span", {
            class: "count-move",
            "data-dir": move > 0 ? "up" : "down",
            // The sign is in the text, not only in the colour: the badge has to say
            // which way it went to a reader who does not see the red or the green.
            text: (move > 0 ? "+" : "-") + group(Math.abs(move))
        });
        // Gone from the tree once it has faded, not merely transparent: a screen
        // reader would otherwise still announce a badge nobody can see any more.
        badge.addEventListener("animationend", function () {
            badge.remove();
        });
        return badge;
    }

    function actionRow(run, spec) {
        const row = el("div", {class: "outcome-actions"}, [actionButton(spec.primary, true)]);
        if (spec.secondary) row.appendChild(actionButton(spec.secondary, false));
        if (spec.note) {
            const note = typeof spec.note === "function" ? spec.note : null;
            row.appendChild(live(el("span", {class: "outcome-note", text: note ? note() : spec.note}), note));
        }
        return row;
    }

    function actionButton(spec, primary) {
        const className = "action" + (primary && spec.filled ? " action-filled" : "")
            + (primary ? "" : " action-secondary");
        if (spec.href) return el("a", {class: className, href: spec.href, text: spec.label});
        const button = el("button", {type: "button", class: className, text: spec.label});
        button.addEventListener("click", function () {
            spec.action(button);
        });
        return button;
    }

    function resubmit(run, button) {
        fetch("/api/analyses", {
            method: "POST",
            headers: {"content-type": "application/json"},
            body: JSON.stringify({source_id: run.source_id, request: run.request || {}})
        }).then(function (response) {
            return response.json().then(function (payload) {
                return {ok: response.ok, payload: payload};
            });
        }).then(function (result) {
            if (!result.ok || !result.payload.id) {
                throw new Error(result.payload.detail || "The Hub refused the resubmission.");
            }
            location.hash = "#/run/" + result.payload.id;
        }).catch(function (error) {
            // Silence here reads as a broken button: the operator clicked and
            // nothing moved. The note carries the run's arguments, so the error
            // borrows the line and hands it back.
            const note = button.parentNode && button.parentNode.querySelector(".outcome-note");
            if (!note) return;
            // Stashed on the node, not in a closure: a second failure inside the
            // window would otherwise capture the first error as the text to restore
            // and pin it there for good.
            if (note.dataset.restore === undefined) note.dataset.restore = note.textContent;
            clearTimeout(state.noteTimer);
            note.textContent = String(error.message || error);
            note.setAttribute("data-error", "true");
            state.noteTimer = setTimeout(function () {
                note.textContent = note.dataset.restore;
                delete note.dataset.restore;
                note.removeAttribute("data-error");
            }, 6000);
        });
    }

    function loadRun(id) {
        return getJson("/api/analyses/" + id).then(function (run) {
            state.run = run;
            state.runError = false;
            render();
            if (run.status === "pending" || run.status === "running") scheduleRunPoll(id);
        }).catch(function () {
            state.runError = true;
            render();
        });
    }

    function scheduleRunPoll(id) {
        clearTimeout(state.runTimer);
        state.runTimer = setTimeout(function () {
            if (currentRunId() === id) loadRun(id);
        }, 1000);
    }


    // ------------------------------------------------------- advanced: detection

    /**
     * One sentence per knob, saying what the detector stops seeing when the
     * number goes up. Written in the terms of what is looked for, never in terms
     * of file size: raising a threshold does not shorten a report, it decides
     * that a smaller pattern is no longer a problem.
     */
    const DETECTION_COPY = {
        n_plus_one_min_occurrences: "How many near-identical queries in one trace count as an N+1. "
            + "Raise it and smaller loops stop being reported at all.",
        window_duration_ms: "How close together those queries have to be. A shorter window splits one "
            + "slow loop into several groups that each fall under the count.",
        slow_query_threshold_ms: "Above this, one operation is called slow.",
        slow_query_min_occurrences: "How many times a slow template has to appear before it is worth "
            + "reporting. One slow query stays invisible below this.",
        max_fanout: "Child spans under one parent before it counts as excessive fanout. The engine "
            + "warns outside 5 to 1 000: too low floods the list, too high hides real fan-outs.",
        chatty_service_min_calls: "Outbound HTTP calls in one trace before a service is called chatty. "
            + "Critical fires at three times this.",
        pool_saturation_concurrent_threshold: "Peak concurrent SQL spans on one service before the "
            + "connection pool is called at risk. Set it to the pool size you actually run.",
        serialized_min_sequential: "Sequential independent calls under one parent before they are "
            + "worth parallelising.",
        sanitizer_aware_classification: "How a run of identical parameterised queries is read once the "
            + "agent has hidden their literals. auto calls it an N+1 on the ORM scope alone, strict also "
            + "wants the timings to spread, never leaves it a redundant query, always reports an N+1.",
        sanitizer_aware_min_cv: "How much those timings have to spread (standard deviation over "
            + "mean) before strict or auto call the run an N+1 rather than a cached repeat. The same bar "
            + "reads a repeated HTTP call. Raise it on a jittery runtime such as PHP-FPM, where repeats "
            + "of one cached query spread past 0.5."
    };

    function detectionKnobs() {
        return (state.status && state.status.detection_knobs) || [];
    }

    function detectionCount() {
        return Object.keys(state.form.detection).length;
    }

    function setDetection(name, raw, knob) {
        // A choice stays the word it is, a threshold becomes the number it reads as.
        const value = knob.kind === "choice" ? raw : Number(raw);
        const unreadable = knob.kind !== "choice" && !Number.isFinite(value);
        // An empty field or the engine's own default is not an override: recording
        // it would make the run card claim a departure that never happened.
        if (raw === "" || unreadable || value === knob.default) delete state.form.detection[name];
        else state.form.detection[name] = value;
        updateSubmit();
        refreshDetectionCount();
    }

    function refreshDetectionCount() {
        const badge = document.getElementById("advanced-count");
        if (!badge) return;
        const count = detectionCount();
        badge.hidden = count === 0;
        badge.textContent = count === 1 ? "1 changed" : count + " changed";
        const resetAll = document.getElementById("advanced-reset");
        // Nothing to put back when nothing was moved, and a button that does
        // nothing is a button that has to be tried to be understood.
        if (resetAll) resetAll.hidden = count === 0;
    }

    /**
     * A disclosure, and the only one in this form. It holds settings that
     * change what the analysis looks for, which is a different question from
     * every other control on this screen, so it is folded away rather than
     * mixed in.
     */
    function advancedPanel() {
        const knobs = detectionKnobs();
        if (knobs.length === 0) return null;

        const summary = el("summary", {class: "advanced-summary"}, [
            warningGlyph(14),
            // The glyph is aria-hidden, so the caution it carries needs words a
            // screen reader receives while the panel is still collapsed.
            el("span", {class: "visually-hidden", text: "Warning, expert settings."}),
            el("span", {class: "overline"}, [
                el("span", {class: "over-warn", text: "// advanced users only"}),
                document.createTextNode(" \u00b7 what the analysis looks for")
            ]),
            el("span", {id: "advanced-count", class: "advanced-count", hidden: "hidden"})
        ]);

        const body = el("div", {class: "advanced-body"}, [
            el("section", {class: "notice-block", "data-tone": "warn"}, [
                warningGlyph(17),
                el("div", {class: "notice-block-text"}, [
                    el("p", {
                        class: "notice-block-title",
                        text: "For operators who know what these thresholds do."
                    }),
                    el("p", {
                        class: "notice-block-body",
                        text: "Moved carelessly, they hide real problems or flood the report with noise, and "
                            + "which direction does which differs per threshold. If you are not sure, leave them."
                    })
                ])
            ]),
            el("p", {
                class: "advanced-lead",
                text: "These are the engine's detection thresholds. They decide what counts as a problem, "
                    + "not how the report is written: raising one does not make the run lighter, it makes the "
                    + "engine stop reporting the smaller cases. A run records the ones you changed, and the "
                    + "recent list flags counts that came from different thresholds, because they are not "
                    + "comparable."
            })
        ]);

        // One button for the lot, beside the count so the two agree at a glance.
        const resetAll = el("button", {
            type: "button",
            id: "advanced-reset",
            class: "pill-button pill-sm advanced-reset",
            text: "Reset every threshold"
        });
        resetAll.addEventListener("click", function () {
            state.form.detection = {};
            updateSubmit();
            render();
        });
        body.appendChild(el("div", {class: "advanced-actions"}, [resetAll]));

        knobs.forEach(function (knob) {
            body.appendChild(detectionRow(knob));
        });

        const panel = el("details", {class: "advanced"}, [summary, body]);
        // Open because the reader left it open, or because a threshold in it is
        // set and hiding that would hide what the run is about to do.
        panel.open = state.panelOpen.advanced === true || detectionCount() > 0;
        panel.addEventListener("toggle", function () {
            // Only a change the reader made: every render rebuilds this panel and
            // sets `open` above, which fires this same event.
            if (state.panelOpen.advanced === panel.open) return;
            state.panelOpen.advanced = panel.open;
            saveFolds();
        });
        queueMicrotask(refreshDetectionCount);
        return panel;
    }

    function detectionRow(knob) {
        const current = state.form.detection[knob.name];
        const identifier = "knob-" + knob.name;
        // The default as a value, not as a placeholder. Empty, the field had
        // nothing for the spinner to step from, so the up arrow jumped to the
        // minimum: 10 became 2. It stays in the muted tone until it is moved,
        // and a value equal to the default is still not an override.
        const shown = current === undefined ? String(knob.default) : String(current);
        const input = knob.kind === "choice"
            ? el("select", {id: identifier, class: "input input-knob"}, knob.choices.map(function (choice) {
                return el("option", {value: choice, text: choice});
            }))
            : el("input", {
                id: identifier,
                type: "number",
                class: "input input-knob",
                min: String(knob.min),
                max: String(knob.max),
                // A decimal steps by a hundredth, an integer by one.
                step: knob.kind === "decimal" ? "0.01" : "1",
                value: shown
            });
        // A select takes its value once its options exist, not as an attribute.
        if (knob.kind === "choice") input.value = shown;
        const label = "Put " + knob.name + " back to " + knob.default;
        const reset = el("button", {
            type: "button",
            class: "knob-reset",
            // The glyph is aria-hidden like every other in the product, so the button
            // carries the words itself, and a title says them to a mouse as well.
            "aria-label": label,
            title: label
        }, [undoGlyph(14)]);

        function mark() {
            const moved = state.form.detection[knob.name] !== undefined;
            input.toggleAttribute("data-default", !moved);
            reset.hidden = !moved;
        }

        mark();

        input.addEventListener("input", function () {
            setDetection(knob.name, input.value, knob);
            mark();
        });
        reset.addEventListener("click", function () {
            input.value = String(knob.default);
            setDetection(knob.name, input.value, knob);
            mark();
        });

        return el("div", {class: "knob"}, [
            el("label", {class: "knob-head", for: identifier}, [
                el("span", {class: "knob-name", text: knob.name}),
                el("span", {class: "knob-default", text: "default " + knob.default})
            ]),
            el("span", {class: "knob-body", text: DETECTION_COPY[knob.name] || ""}),
            el("div", {class: "knob-controls"}, [input, reset])
        ]);
    }

    // ---------------------------------------------------- screen: recent runs

    function renderRecentScreen() {
        const section = el("section", {}, [
            ruledOverline("// recent analyses"),
            el("h1", {class: "page-title", text: "The team's short memory"}),
            el("p", {
                class: "page-sub",
                text: "Reports are deleted " + state.status.limits.report_retention_hours + " hours after they "
                    + "succeed. This is not an audit trail, and a link you shared yesterday is already dead."
            })
        ]);

        if (!state.runs) {
            section.appendChild(el("div", {class: "card skeleton", style: "height:120px;margin-top:18px"}));
            return section;
        }
        if (state.runs.length === 0) {
            section.appendChild(el("div", {class: "empty-state"}, [
                el("p", {class: "empty-title", text: "Nothing here yet."}),
                el("p", {
                    text: "Not “no results”. This list is the team's short memory, and after "
                        + state.status.limits.report_retention_hours + " idle hours retention returns it to "
                        + "exactly this state. That is normal, so it reads as normal."
                })
            ]));
            return section;
        }

        const binaries = Array.from(new Set(
            state.runs.map(function (run) {
                return run.producer_version;
            }).filter(Boolean))).sort(PSL.vcmp);
        if (binaries.length > 1) {
            section.appendChild(el("div", {class: "banner", "data-tone": "warn"}, [
                svg([["path", {d: "M10.3 3.9 1.9 18a2 2 0 0 0 1.7 3h16.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"}],
                    ["path", {d: "M12 9v4M12 17h.01"}]], 16),
                el("p", {
                    text: "These analyses were produced by " + binaries.join(" and ") + ". Counts from "
                        + binaries.length + " binaries are not directly comparable: a detector added between "
                        + "minors changes what gets found, not only how much. The label on each card names which "
                        + "binary did the detecting."
                })
            ]));
        }

        const tuned = state.runs.filter(function (run) {
            return Object.keys((run.request || {}).detection || {}).length > 0;
        });
        if (tuned.length > 0 && tuned.length < state.runs.length) {
            section.appendChild(el("div", {class: "banner", "data-tone": "warn"}, [
                warningGlyph(16),
                el("p", {
                    text: tuned.length + (tuned.length === 1 ? " run" : " runs") + " here changed the "
                        + "detection thresholds. Their counts are not comparable with the rest: a threshold "
                        + "decides what gets reported, so a lower count can mean a quieter service or simply "
                        + "a detector that was told to look for less. Each card names the thresholds it used."
                })
            ]));
        }

        section.appendChild(legendStrip());
        section.appendChild(el("div", {class: "run-list"}, state.runs.map(runCard)));
        return section;
    }

    function legendStrip() {
        const keys = ["queued", "running", "succeeded", "empty", "failed", "interrupted", "expired"];
        const strip = el("div", {class: "legend"}, [el("span", {class: "overline", text: "legend"})]);
        keys.forEach(function (key) {
            strip.appendChild(el("span", {
                class: "status-pill",
                "data-status": key,
                text: key === "empty" ? "succeeded · empty" : key
            }));
        });
        return strip;
    }

    function runCard(run) {
        const key = PSL.statusKey(run);
        const card = el("a", {class: "run-card", "data-status": key, href: "#/run/" + run.id});

        card.appendChild(el("span", {class: "run-card-line"}, [
            el("span", {class: "status-pill", "data-status": key, text: key === "empty" ? "succeeded · empty" : key}),
            el("span", {class: "run-card-name", text: run.source_name}),
            el("span", {class: "chip", text: PSL.KIND_LABEL[run.kind] || run.kind}),
            el("span", {
                class: "chip chip-declared",
                text: run.environment,
                title: "Declared by the source's configuration, not measured."
            }),
            el("span", {class: "run-card-spacer"}),
            el("span", {class: "run-card-id", text: run.id})
        ]));
        card.appendChild(el("span", {class: "run-card-args", text: PSL.argsLine(run), title: PSL.argsLine(run)}));
        card.appendChild(el("span", {class: "run-card-facts"}, cardFacts(run, key).map(function (fact) {
            return el("span", {class: "fact"}, [
                el("span", {class: "fact-k", text: fact[0]}),
                live(el("span", {class: "fact-v", "data-tone": fact[2] || "mono", text: fact[1]}), fact[3])
            ]);
        })));
        return card;
    }

    /** Durations relative to now, the way an operator reads a list: "3 s", not a clock. */
    function cardFacts(run, key) {
        const now = Date.now();
        const started = run.started_at_ms || run.created_at_ms;
        const ran = run.finished_at_ms
            ? PSL.dur(run.finished_at_ms - started)
            : key === "queued" ? "not started" : PSL.dur(now - started) + " so far";
        const facts = [["by", run.requested_by], ["ran", ran]];
        if (run.producer_version) facts.push([PSL.detector(run.kind), run.producer_version,
            PSL.skew(run.producer_version) ? "warn" : "mono"]);
        facts.push(["started", PSL.dur(now - started) + " ago"]);
        facts.push(["expires", run.expires_at_ms ? expiryText(run) : "n/a",
            run.expires_at_ms && run.expires_at_ms < now ? "crit" : "mono",
            run.expires_at_ms ? function () {
                return expiryText(run);
            } : null]);
        const tuned = Object.keys((run.request || {}).detection || {});
        if (tuned.length > 0) {
            facts.push(["thresholds", tuned.length === 1 ? "1 changed" : tuned.length + " changed", "warn"]);
        }
        if (run.error_code) facts.push(["error", run.error_code, "crit"]);
        return facts;
    }

    function loadRuns() {
        // limit=500 rather than the API's 50 default: the weight history
        // filters per source, and on a busy multi-source Hub the newest 50
        // can all belong to someone else.
        return getJson("/api/analyses?limit=500").then(function (runs) {
            state.runs = runs;
            applyRuns();
        }).catch(function () {
            state.runs = [];
            applyRuns();
        });
    }

    /**
     * On the launcher, refresh the weight-history block in place instead of
     * re-rendering: the deferred fetch would otherwise replace the whole
     * form mid-interaction and drop focus, caret and slider capture, the
     * same hazard the slider handler documents.
     */
    function applyRuns() {
        if (currentScreen() !== "new") {
            render();
            return;
        }
        const slot = document.getElementById("weight-history");
        if (!slot) return;
        slot.replaceChildren();
        const history = weightHistory();
        if (history) slot.appendChild(history);
    }

    // ------------------------------------------------ screen: dashboard handoff

    /**
     * The report is served byte for byte as the engine produced it, in a frame of
     * its own. The surface changes visibly so the operator knows they left the
     * launcher, and the single return is always present.
     */
    function renderReportScreen(id) {
        const frame = el("iframe", {class: "report-frame", src: "/reports/" + id + ".html", title: "Analysis report"});
        const lifetime = live(el("span", {class: "report-engine", text: reportLifetime(id)}),
            function () {
                return reportLifetime(id);
            });
        const bar = el("div", {class: "report-bar"}, [
            el("a", {class: "pill-button", href: "#/run/" + id}, [
                svg([["path", {d: "M14 6l-6 6 6 6"}]], 14),
                el("span", {text: "Back to the launcher"})
            ]),
            el("span", {class: "report-path", text: "report / " + id}),
            el("span", {class: "report-spacer"}),
            lifetime
        ]);
        return el("div", {class: "report-shell"}, [bar, frame]);
    }

    function reportLifetime(id) {
        const run = state.run && state.run.id === id ? state.run : null;
        const version = state.status && state.status.engine_version;
        const rendered = "Rendered by perf-sentinel " + (version || "unknown");
        if (!run || !run.expires_at_ms) return rendered;
        const left = run.expires_at_ms - Date.now();
        return rendered + (left > 0 ? " · expires in " + PSL.durPrecise(left) : " · expired");
    }

    // ------------------------------------------------------------------ boot

    initTheme();
    // Before the first render, so a row left open comes back open and reads its
    // daemon on its own rather than waiting to be clicked again.
    restoreFolds();
    restoreShell();
    // Escape closes the picker. Without it the only ways out are Apply, a quick
    // range or a click outside, and a keyboard user has none of them.
    globalThis.addEventListener("keydown", function (event) {
        if (event.key === "Escape" && state.form.pickerOpen) {
            state.form.pickerOpen = false;
            render();
        }
    });
    render();
    loadShell();
    globalThis.addEventListener("hashchange", onRoute);
})();
