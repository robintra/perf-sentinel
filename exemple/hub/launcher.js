/**
 * perf-sentinel Hub launcher — pure logic.
 *
 * Classic script, no module syntax, no build step: the page loads it with a plain
 * <script src> and reads it off `window.PSL`. Brief §3.5 forbids a build stage in
 * the shipped app, so this file is authored as runnable JavaScript and type-checked
 * separately with `tsc --noEmit` against `types.d.ts`, which `npm run typecheck`
 * at the repository root runs.
 *
 * Everything here is a pure function or a frozen table. Rendering, state and DOM
 * live in the page.
 */
(function (global) {
    'use strict';

    /**
     * The analysis binary embedded in the Hub, and the Hub service itself. Both
     * come from `/api/status` at load time rather than being baked in: a value
     * frozen here would go stale the first time either side is upgraded, and
     * `skew()` would then compare against a version nobody is running.
     */
    let ENGINE = null;
    let HUB = null;

    /**
     * @param {string | null} hub
     * @param {string | null} engine
     * @returns {void}
     */
    function setVersions(hub, engine) {
        HUB = hub;
        ENGINE = engine;
    }

    /**
     * One actionable sentence per code, naming the next action. The service refuses to
     * expose raw stderr, so this table is the entire failure vocabulary the operator gets.
     * @type {Record<import('../types').ErrorCode, string>}
     */
    const ERRORS = {
        source_unreachable: 'nothing answered at its address. Check the daemon or backend is up and that the Hub still has a route to it, then run this again.',
        source_auth_failed: 'it answered and refused the Hub\'s credentials. Rotate the configured auth header or API key in the source\'s Secret. Nothing here will work until that is done.',
        source_rejected_request: 'it answered and refused these arguments, usually an unknown service name or a window it does not keep. Check the service against the backend and try a shorter lookback.',
        timeout: 'the run passed this Hub\'s time ceiling, or the backend took too long to answer. A wide window is the usual cause, halve the trace cap or shorten it. If it repeats instantly whatever the window, the address is probably not answering at all, check the endpoint.',
        output_too_large: 'the source returned more than one run is allowed to hold. Narrow the window or lower the trace cap so less comes back, then run it again.',
        binary_failed: 'the analysis binary failed for a reason none of the other codes covers, and nothing was stored. Run it once more. If it repeats, send this analysis ID to whoever operates the Hub.',
        invalid_request: 'the arguments were rejected before the run started, so nothing was read and nothing was spent. Fix the trace ID or the lookback value and submit again.',
        internal: 'the Hub itself failed and never touched the source. Retry now. If it fails the same way, the Hub needs attention rather than your request.'
    };

    /**
     * What a failed read of a source means. A different vocabulary from ERRORS,
     * which describes a failed analysis run: these are the codes the collector
     * records, and only the ones a settings read can actually produce.
     */
    const READ_ERRORS = {
        network_error: 'nothing answered at its address. Check the daemon is up and that the Hub still has a route to it.',
        http_error: 'it answered with an error status, so it is running and reachable. This is the daemon refusing or failing the request rather than a network problem.',
        timeout: 'it did not answer inside this Hub\'s HTTP timeout. Busy is as likely as down.',
        invalid_status: 'it answered, but not with a status this Hub can read. Its /api/status has to carry a version string.',
        response_too_large: 'it answered with more than this Hub reads in one go. A [daemon] section past that cap is worth reporting.',
        hub_busy: 'the Hub capped how many daemon reads run at once and this one hit the cap. It clears in about a second.'
    };

    /** @type {Record<import('../types').ErrorCode, string>} */
    const ERROR_TITLES = {
        source_unreachable: 'No connection could be opened.',
        source_auth_failed: 'The source rejected the Hub\'s credentials.',
        source_rejected_request: 'The source refused the request.',
        timeout: 'The run exceeded the execution ceiling.',
        output_too_large: 'The source returned too much data.',
        binary_failed: 'The analysis binary failed.',
        invalid_request: 'The arguments were rejected before the run started.',
        internal: 'The Hub failed on its own side.'
    };

    /** @type {Record<import('../types').SourceKind, string>} */
    const KIND_LABEL = {daemon: 'daemon', tempo: 'tempo', jaeger_query: 'victoria traces'};

    const UNIT_MS = {m: 60000, h: 3600000, d: 86400000};
    const UNIT_WORD = {m: 'minute', h: 'hour', d: 'day'};

    /**
     * Coarse duration. Drops a zero remainder so an exact hour reads "1 h", not "1 h 0 m".
     * @param {number | null | undefined} ms
     * @returns {string}
     */
    function dur(ms) {
        if (ms == null) return 'n/a';
        const s = Math.max(0, Math.round(ms / 1000));
        if (s < 60) return s + ' s';
        const m = Math.floor(s / 60);
        if (m < 60) return s % 60 ? m + ' m ' + (s % 60) + ' s' : m + ' m';
        const h = Math.floor(m / 60);
        if (h < 24) return m % 60 ? h + ' h ' + (m % 60) + ' m' : h + ' h';
        const d = Math.floor(h / 24);
        return h % 24 ? d + ' d ' + (h % 24) + ' h' : d + ' d';
    }

    /**
     * Every unit from the largest that applies down to `floor`, keeping the ones
     * in between even when they are zero. dur() stops at two units, which reads
     * well for an age skimmed in a table and badly for a figure someone watches:
     * "10 d" hides a whole day of drift, and a countdown never appears to move.
     * @param {number | null | undefined} ms
     * @param {'s' | 'm'} floor
     * @returns {string}
     */
    function durDown(ms, floor) {
        if (ms == null) return 'n/a';
        const s = Math.max(0, Math.round(ms / 1000));
        const d = Math.floor(s / 86400);
        const h = Math.floor(s / 3600) % 24;
        const m = Math.floor(s / 60) % 60;
        const parts = [];
        if (d) parts.push(d + ' d');
        if (d || h) parts.push(h + ' h');
        if (d || h || m || floor === 'm') parts.push(m + ' m');
        if (floor === 's') parts.push((s % 60) + ' s');
        return parts.join(' ');
    }

    /**
     * Splits a fleet into the two kinds the launcher groups by, keeping each
     * source's position in the original array so a caller can still address a row
     * by its global index. `split` is false for a single-kind fleet, where a group
     * label would name the only thing on screen.
     * @param {{kind?: string}[]} sources
     * @returns {{daemons: {source: any, index: number}[],
     *            backends: {source: any, index: number}[], split: boolean}}
     */
    function splitByKind(sources) {
        const daemons = [];
        const backends = [];
        (sources || []).forEach(function (source, index) {
            (source && source.kind === 'daemon' ? daemons : backends).push({source: source, index: index});
        });
        return {daemons, backends, split: daemons.length > 0 && backends.length > 0};
    }

    /** A countdown, watched: down to the second. */
    function durPrecise(ms) {
        return durDown(ms, 's');
    }

    /** An uptime, re-read on an interval: down to the minute, seconds would lie. */
    function durMinutes(ms) {
        return durDown(ms, 'm');
    }

    /**
     * Same duration, split into figure and unit so a caller can set them at different
     * sizes. A 40px "m" beside a 40px digit reads as two glyphs, not one duration.
     * @param {number | null | undefined} ms
     * @returns {{n: string, u: string}[]}
     */
    function durParts(ms) {
        const s = dur(ms);
        if (s === 'n/a') return [{n: s, u: ''}];
        /** @type {{n: string, u: string}[]} */
        const out = [];
        const re = /(\d+)\s*([a-z]+)/g;
        let m;
        while ((m = re.exec(s)) !== null) out.push({n: m[1] ?? '', u: m[2] ?? ''});
        return out;
    }

    /**
     * Local wall clock with milliseconds, for the event log.
     * @param {number} ms
     * @returns {string}
     */
    function clock(ms) {
        const d = new Date(ms);
        const p = (/** @type {number} */ n, /** @type {number} */ w) => String(n).padStart(w, '0');
        return p(d.getHours(), 2) + ':' + p(d.getMinutes(), 2) + ':' + p(d.getSeconds(), 2) + '.' + p(d.getMilliseconds(), 3);
    }

    /**
     * Relative window string, e.g. `15m`, `6h`, `90d`. Unparseable input falls back to
     * one hour rather than throwing: this feeds a live preview, not a submission.
     * @param {string} s
     * @returns {number}
     */
    function parseDur(s) {
        const m = /^(\d+)([mhd])$/.exec(s || '');
        return m ? Number(m[1]) * UNIT_MS[/** @type {'m'|'h'|'d'} */ (m[2])] : 3600000;
    }

    /**
     * @param {string} s
     * @returns {string}
     */
    function humanDur(s) {
        const m = /^(\d+)([mhd])$/.exec(s || '');
        if (!m) return s;
        const n = Number(m[1]);
        return n + ' ' + UNIT_WORD[/** @type {'m'|'h'|'d'} */ (m[2])] + (n > 1 ? 's' : '');
    }

    /**
     * Value for an `<input type="datetime-local">`, in local time as that control expects.
     * @param {number} ms
     * @returns {string}
     */
    function dtLocal(ms) {
        const d = new Date(ms);
        const p = (/** @type {number} */ n) => String(n).padStart(2, '0');
        return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) + 'T' + p(d.getHours()) + ':' + p(d.getMinutes());
    }

    /**
     * @param {number} ms
     * @returns {string}
     */
    function dtHuman(ms) {
        const d = new Date(ms);
        const p = (/** @type {number} */ n) => String(n).padStart(2, '0');
        return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) + ' ' + p(d.getHours()) + ':' + p(d.getMinutes());
    }

    /**
     * @param {string | null | undefined} v
     * @returns {number[]}
     */
    function vparts(v) {
        return String(v || '').split('.').map(n => Number(n) || 0);
    }

    /**
     * Ordering over `major.minor.patch`. Extra segments are ignored: the Hub's own
     * version has four, and only the first three ever carry meaning here.
     * @param {string | null | undefined} a
     * @param {string | null | undefined} b
     * @returns {-1 | 0 | 1}
     */
    function vcmp(a, b) {
        const A = vparts(a), B = vparts(b);
        for (let i = 0; i < 3; i++) {
            const x = A[i] ?? 0, y = B[i] ?? 0;
            if (x !== y) return x < y ? -1 : 1;
        }
        return 0;
    }

    /**
     * The first segment where two versions differ, and by how much.
     *
     * `vcmp` decides the direction on all three segments, so the label has to
     * name the segment it decided on. Reading the minor alone worded a patch
     * gap as "0 minor behind" and a major gap in minors.
     *
     * @param {string | null | undefined} a
     * @param {string | null | undefined} b
     * @returns {{count: number, unit: string}}
     */
    function versionGap(a, b) {
        const units = ['major', 'minor', 'patch'];
        const pa = vparts(a);
        const pb = vparts(b);
        for (let i = 0; i < units.length; i++) {
            const d = Math.abs((pb[i] ?? 0) - (pa[i] ?? 0));
            if (d !== 0) return {count: d, unit: units[i]};
        }
        // `vcmp` reads the same three segments, so a caller that already
        // ruled out equality never lands here.
        return {count: 0, unit: 'patch'};
    }

    /**
     * How a producer sits against the Hub's embedded binary.
     *
     * This compares two version strings and nothing more. It cannot know whether a
     * given minor actually changed detection, so callers must word the result as
     * "may not be comparable", never as "incompatible". perf-sentinel is pre-1.0,
     * which is what makes a single minor worth surfacing at all.
     *
     * @param {string | null | undefined} producer
     * @returns {{dir: 'behind' | 'ahead', label: string, fg: string, bg: string, bd: string} | null}
     */
    function skew(producer) {
        // With no engine version there is nothing to compare against, and a
        // comparison against null would read every producer as ahead.
        if (!producer || !ENGINE) return null;
        const c = vcmp(producer, ENGINE);
        if (c === 0) return null;
        const g = versionGap(producer, ENGINE);
        return c < 0
            ? {
                dir: 'behind',
                label: g.count + ' ' + g.unit + ' behind',
                fg: 'var(--warn-fg)',
                bg: 'var(--warn-bg)',
                bd: 'var(--warn-bd)'
            }
            : {
                dir: 'ahead',
                label: g.count + ' ' + g.unit + ' ahead',
                fg: 'var(--info-fg)',
                bg: 'var(--info-bg)',
                bd: 'var(--info-bd)'
            };
    }

    /**
     * Which version string a result of this kind actually carries.
     *
     * A daemon detects its own findings, so it carries `producer`. A trace backend
     * detects nothing: the Hub's embedded binary does, so a historical read carries
     * `engine`. Labelling both "engine" is the §7.6 confusion.
     *
     * @param {import('../types').SourceKind | import('../types').Analysis} kindOrAnalysis
     * @returns {'producer' | 'engine'}
     */
    function detector(kindOrAnalysis) {
        const k = typeof kindOrAnalysis === 'string' ? kindOrAnalysis : kindOrAnalysis.kind;
        return k === 'daemon' ? 'producer' : 'engine';
    }

    /**
     * Presentation status. `empty` is derived here and never stored: it must not
     * become a seventh value of `analysis_runs.status`.
     * @param {import('../types').Analysis} a
     * @returns {import('../types').DisplayStatus}
     */
    function statusKey(a) {
        if (a.status === 'succeeded' && a.result && a.result.empty) return 'empty';
        if (a.status === 'pending') return 'queued';
        return a.status;
    }

    /**
     * One-line restatement of what was asked for. Long values are truncated by CSS,
     * never here: the full string stays available in a `title`.
     * @param {import('../types').Analysis} a
     * @returns {string}
     */
    function argsLine(a) {
        const r = /** @type {Record<string, unknown>} */ (a.request || {});
        /** @type {string[]} */
        const parts = [];
        if (r['service']) parts.push('service = ' + r['service']);
        if (r['trace_id']) parts.push('trace_id = ' + r['trace_id']);
        if (r['lookback']) parts.push('lookback = ' + r['lookback']);
        if (r['from_ms']) parts.push('from_ms = ' + r['from_ms']);
        if (r['to_ms']) parts.push('to_ms = ' + r['to_ms']);
        if (r['max_traces'] != null) parts.push('max_traces = ' + r['max_traces']);
        const detection = /** @type {Record<string, number|string>} */ (r['detection'] || {});
        Object.keys(detection).forEach(function (name) {
            parts.push(name + ' = ' + detection[name]);
        });
        return parts.length ? parts.join('   ·   ') : 'no parameters  ·  daemon in-memory snapshot';
    }

    /**
     * Report-weight advice for a requested trace count.
     *
     * Bands come from the report sink (`crates/sentinel-core/src/report/html/mod.rs`):
     * it targets `DEFAULT_SIZE_TARGET_BYTES` (5 MiB) and trims to fit, findings
     * critical-first past `FINDINGS_BUDGET_SHARE_PCT` (70 %) of the budget, then
     * embedded traces lowest-waste-first. The byte size cannot be predicted before the
     * run because it depends on span counts and SQL template lengths, so these are
     * advice and not a bound. Only `ceiling` asks the operator to confirm; only `over`
     * is refused, and that refusal is the service's.
     *
     * The 500 and 1 200 boundaries come from the sink's own 5 MiB target and do
     * not move. The refusal boundary does: it is the service's configured cap,
     * and hardcoding 2 000 would refuse runs a differently configured Hub accepts.
     *
     * @param {number} n
     * @param {number} [cap]
     * @returns {{key: import('../types').WeightBand, label: string, fg: string, bg: string, bd: string, body: string, needsAck: boolean}}
     */
    function weightBand(n, cap) {
        const hardCap = typeof cap === 'number' && cap > 0 ? cap : 2000;
        if (!Number.isFinite(n) || n < 1) {
            return {
                key: 'invalid', label: 'not a count',
                fg: 'var(--crit-fg)', bg: 'var(--crit-bg)', bd: 'var(--crit-bd)', needsAck: false,
                body: 'A run needs at least one trace. Drag the handle or type a number between 1 and '
                    + hardCap + '.'
            };
        }
        if (n <= Math.min(500, hardCap)) {
            return {
                key: 'safe', label: 'comfortable',
                fg: 'var(--ok-fg)', bg: 'var(--ok-bg)', bd: 'var(--ok-bd)', needsAck: false,
                body: 'Well inside what the sink returns whole. At this count the report is bounded by how much your traffic is doing wrong, not by the sink.'
            };
        }
        if (n <= Math.min(1200, hardCap)) {
            return {
                key: 'heavy', label: 'heavy',
                fg: 'var(--warn-fg)', bg: 'var(--warn-bg)', bd: 'var(--warn-bd)', needsAck: false,
                body: 'More traces means more findings, and every one of them reaches the report. The span trees stop at the embed cap, so the extra weight here is the list itself, not the trees.'
            };
        }
        if (n <= hardCap) {
            return {
                key: 'ceiling', label: 'at the ceiling',
                fg: 'var(--crit-fg)', bg: 'var(--crit-bg)', bd: 'var(--crit-bd)', needsAck: true,
                body: 'At this count the report still keeps every finding, and that is what makes it heavy: the file has no fixed ceiling and takes a moment to open. The run is also long enough to be worth watching against this Hub\'s time limit.'
            };
        }
        return {
            key: 'over', label: 'above the hard cap',
            fg: 'var(--crit-fg)', bg: 'var(--crit-bg)', bd: 'var(--crit-bd)', needsAck: false,
            body: 'The service rejects this before the run starts. Nothing is read and nothing is spent.'
        };
    }

    /**
     * A file size in the unit an operator reads. One decimal at most: the second
     * would claim a precision a report does not have, its weight moves with what
     * the run found.
     * @param {number} n
     */
    function bytes(n) {
        if (!Number.isFinite(n) || n < 0) return '';
        if (n < 1024) return n + ' B';
        const kb = Math.round(n / 1024);
        // Rounding can carry into the next unit: 1023.5 KiB must read
        // "1.0 MB", never "1024 KB".
        if (kb < 1024) return kb + ' KB';
        return (n / (1024 * 1024)).toFixed(1) + ' MB';
    }

    /**
     * A value the way a POSIX shell reads it back, byte for byte.
     *
     * Single quotes and never double: inside double quotes a shell still expands
     * `$`, backticks and backslashes, so a service name carrying one would run as
     * something other than what is displayed. Nothing expands inside single
     * quotes, and the only character needing care is the quote itself, closed and
     * reopened around an escaped one.
     *
     * The bare form is kept for values drawn entirely from the set no shell
     * treats specially, because quoting `order-service` would only make the
     * common line harder to read.
     *
     * @param {string} value
     * @returns {string}
     */
    function shq(value) {
        const text = String(value);
        if (text !== '' && /^[A-Za-z0-9_@%+=:,./-]+$/.test(text)) return text;
        return '\'' + text.replace(/'/g, '\'\\\'\'') + '\'';
    }

    /**
     * The same job for PowerShell, which quotes by a different rule: inside
     * single quotes everything is literal and a quote is doubled, where a POSIX
     * shell has to close, escape and reopen.
     *
     * The bare-word set is narrower than the POSIX one on purpose. A comma is
     * PowerShell's array operator, so `a,b` would arrive as two arguments, and
     * `@` opens a splat or a hash literal. Both are quoted here rather than
     * trusted.
     *
     * @param {unknown} value
     * @returns {string}
     */
    function psq(value) {
        const text = String(value);
        if (text !== '' && /^[A-Za-z0-9_.:/=+-]+$/.test(text)) return text;
        return '\'' + text.replace(/'/g, '\'\'') + '\'';
    }

    /* One entry per shell the launcher can spell a command for. `wrap` is what
       continues a command on the next line: a backslash in a POSIX shell, a
       backtick in PowerShell. */
    const SHELLS = [
        {id: 'posix', label: 'bash / zsh', wrap: '\\', quote: shq},
        {id: 'powershell', label: 'PowerShell', wrap: '`', quote: psq}
    ];

    /**
     * The line that puts a value in an environment variable, in the shell's own
     * syntax. The two differ by more than a keyword: PowerShell assigns into the
     * `env:` drive and wants the spaces around the equals sign.
     *
     * @param {string} shellId
     * @param {string} name
     * @param {string} value
     * @returns {string}
     */
    function exportLine(shellId, name, value) {
        const shell = shellById(shellId);
        return shell.id === 'powershell'
            ? '$env:' + name + ' = ' + shell.quote(value)
            : 'export ' + name + '=' + shell.quote(value);
    }

    function shellById(id) {
        return SHELLS.find(function (shell) {
            return shell.id === id;
        }) || SHELLS[0];
    }

    /**
     * Which shell to spell a command for when the reader has not chosen one.
     * Windows gets PowerShell, which is what its terminal opens with, and
     * everything else gets the POSIX line.
     *
     * @param {string | null | undefined} platform navigator.platform, or the
     *   userAgentData platform, whichever the browser offers
     * @returns {string}
     */
    function defaultShell(platform) {
        // Anchored, not a substring: "Darwin" contains "win", and a caller passing
        // a Node style platform string would have handed macOS a PowerShell line.
        return /^win/i.test(String(platform || '').trim()) ? 'powershell' : 'posix';
    }

    /**
     * A shell id if it names one, null otherwise. shellById always answers with a
     * shell, which is right for spelling a command and wrong for judging whether
     * a remembered value is still one.
     *
     * @param {string | null | undefined} id
     * @returns {string | null}
     */
    function knownShell(id) {
        return SHELLS.some(function (shell) {
            return shell.id === id;
        }) ? String(id) : null;
    }

    /**
     * Whole seconds, the way the Hub writes them into its own invocation. The
     * printed window and the launched one have to be the same window.
     * @param {number} ms
     * @returns {string}
     */
    function isoUtc(ms) {
        return new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z');
    }

    /**
     * The request as the engine's own command line.
     *
     * Takes the very object the launcher posts, never the form, so the printed
     * command and the submitted run cannot drift: one shape feeds both, and a
     * field added to one without the other goes missing here rather than wrong.
     * Returns null for a daemon, which takes no arguments at all.
     *
     * The break follows the engine's own examples: the subcommand, the endpoint
     * and the selector on the first line, everything else on the second.
     *
     * @param {import('../types').Source} source
     * @param {Record<string, unknown>} request
     * @returns {string | null}
     */
    function analysisCommand(source, request, shellId) {
        if (!source.engine_subcommand) return null;
        const shell = shellById(shellId);
        const shq = shell.quote;
        const head = ['perf-sentinel ' + source.engine_subcommand, '--endpoint ' + shq(source.base_url)];
        /** @type {string[]} */
        const tail = [];
        if (request['trace_id'] != null) {
            head.push('--trace-id ' + shq(String(request['trace_id'])));
        } else {
            head.push('--service ' + shq(String(request['service'] == null ? '' : request['service'])));
            if (request['from_ms'] != null) {
                tail.push('--from ' + isoUtc(Number(request['from_ms'])));
                tail.push('--to ' + isoUtc(Number(request['to_ms'])));
            } else {
                tail.push('--lookback ' + shq(String(request['lookback'])));
            }
            tail.push('--max-traces ' + String(request['max_traces']));
        }
        if (source.auth_header_name) tail.push('--auth-header-env PERF_SENTINEL_SOURCE_TOKEN');
        // Undotted, and named rather than left to the engine's discovery of
        // `.perf-sentinel.toml`, which is dotted and cwd-only. The Hub hands this
        // file over as a download, and a downloaded file may not keep a leading
        // dot, so asking for the dotted name would ask for one the reader might not
        // have. Naming it also makes a missing file stop the run instead of
        // silently reverting to the defaults the reader just moved away from.
        if (Object.keys(request['detection'] || {}).length > 0) tail.push('-c perf-sentinel.toml');
        return tail.length === 0
            ? head.join(' ')
            : head.join(' ') + ' ' + shell.wrap + '\n  ' + tail.join(' ');
    }

    /**
     * The live view of a daemon in a terminal. `--daemon` sits on `query` and not
     * on `monitor`, so the order is not interchangeable.
     * @param {import('../types').Source} source
     * @returns {string}
     */
    /**
     * `--daemon` belongs to the parent `query`, `--refresh` to `monitor`, so the
     * order is not free. The interval is the one the row is already re-reading
     * on: a reader who slowed this screen down means it, and the command they
     * copy should not contradict the screen they copied it from.
     *
     * @param {any} source
     * @param {number | undefined} refreshSeconds omitted, or 0, leaves the engine's own default
     * @param {string} shellId
     * @returns {string}
     */
    function monitorCommand(source, refreshSeconds, shellId) {
        const command = 'perf-sentinel query --daemon '
            + shellById(shellId).quote(source.base_url) + ' monitor';
        return Number.isInteger(refreshSeconds) && refreshSeconds > 0
            ? command + ' --refresh ' + refreshSeconds
            : command;
    }

    /**
     * The overridden thresholds as the file `-c` expects. Only the ones a run
     * actually changed: a value equal to the engine's own default is dropped
     * before it reaches here, so every key present is a real departure. A
     * choice rides as a string and is quoted, a threshold as a number.
     * @param {Record<string, number|string>} detection
     * @returns {string}
     */
    function detectionToml(detection) {
        return ['[detection]'].concat(Object.keys(detection).sort().map(function (name) {
            const value = detection[name];
            // TOML has no bare words: a choice is a quoted string or a parse error.
            return name + ' = ' + (typeof value === 'string' ? JSON.stringify(value) : value);
        })).join('\n');
    }

    /** True when a value had to be quoted, so the block can name the shell. */
    function quotedForShell(command) {
        return command.indexOf('\'') >= 0;
    }

    /**
     * The state a light refresh yields, mirroring DaemonView.Classify with the
     * hints of the page's last full read: a status-only body carries the gauges
     * but not the daemon's hints, and once a minute a full read re-syncs both.
     *
     * @param {{traces: any, analysis_queue: any, findings: any}} view
     * @param {number} warningCount
     * @returns {string}
     */
    function lightState(view, warningCount) {
        const gauges = [view.traces, view.analysis_queue, view.findings];
        if (gauges.some(function (g) {
            return g && g.at_capacity;
        })) return 'near_capacity';
        if (warningCount > 0) return 'advised';
        return gauges.every(function (g) {
            return !g || g.pct === null;
        }) ? 'unknown' : 'ok';
    }

    /**
     * The folds worth remembering: the open ones. A closed fold is the default,
     * so storing it would only grow the record every time a reader tidies up
     * after themselves, and leave the names of sources that no longer exist.
     *
     * @param {Record<string, boolean>} folds
     * @returns {Record<string, true>}
     */
    function openFolds(folds) {
        /** @type {Record<string, true>} */
        const open = {};
        Object.keys(folds || {}).forEach(function (key) {
            if (folds[key] === true) open[key] = true;
        });
        return open;
    }

    /* The engine's own advisor line: at 90 % of a cap the daemon starts saying so
       in its hints, and the row's verdict turns to near capacity. The 75 % step
       below it is the Hub's, a heads-up before the daemon complains rather than a
       second opinion about when it should. */
    const GAUGE_WARN_PCT = 75;
    const GAUGE_CRIT_PCT = 90;

    /**
     * The tone a gauge's own figure takes, or null to leave it in the plain text
     * colour. Only a known percentage is toned: a gauge with no published cap
     * says nothing about how close to one it is.
     *
     * @param {number | null | undefined} pct
     * @returns {'crit' | 'warn' | null}
     */
    function gaugeTone(pct) {
        if (typeof pct !== 'number' || !isFinite(pct)) return null;
        if (pct >= GAUGE_CRIT_PCT) return 'crit';
        if (pct >= GAUGE_WARN_PCT) return 'warn';
        return null;
    }

    /**
     * How far a gauge moved between two reads, or null when there is nothing to
     * show: no earlier reading, a value either side is unknown, or one that did
     * not move at all.
     *
     * @param {any} before
     * @param {any} after
     * @returns {number | null}
     */
    function gaugeMove(before, after) {
        const from = before ? before.value : null;
        const to = after ? after.value : null;
        if (typeof from !== 'number' || typeof to !== 'number' || from === to) return null;
        return to - from;
    }

    const ENGINE_REPOSITORY = 'https://github.com/robintra/perf-sentinel';
    const HUB_REPOSITORY = 'https://github.com/robintra/PerfSentinelHub';
    /* Every chart version with the engine version it ships, which a release tag
       cannot give: a chart-only fix bumps the chart and not the appVersion. */
    const CHART_PAGE = 'https://artifacthub.io/packages/helm/perf-sentinel/perf-sentinel';
    /* Not a link. safeHttpsHref would refuse the scheme, and it is a coordinate
       to paste into helm rather than a page to open. */
    const CHART_COORDINATE = 'oci://ghcr.io/robintra/charts/perf-sentinel';

    /**
     * Where to get the engine a printed command needs. Pinned to the version
     * this Hub runs, since that is the one the command is spelled for. Anything
     * that is not a plain version lands on the release list instead of building
     * a URL out of it: the string comes from a binary's own --version.
     *
     * @param {string | null} version
     * @returns {string}
     */
    function releaseUrl(version) {
        return /^[0-9][0-9A-Za-z.+-]{0,63}$/.test(String(version || ''))
            ? ENGINE_REPOSITORY + '/releases/tag/v' + version
            : ENGINE_REPOSITORY + '/releases';
    }

    /**
     * Where to get a newer Hub. Always the release list: a tag URL would have to
     * be built from a version, and the newest one is what the reader wants.
     *
     * @returns {string}
     */
    function hubReleaseUrl() {
        return HUB_REPOSITORY + '/releases';
    }

    /**
     * Whether a newer release exists, given what is running and what the Hub last
     * read from GitHub.
     *
     * Null covers every case where there is nothing to say, and they are not the
     * same case: the check is off or has not run (latest null), the product is
     * not configured at all (current null), or the two match. A caller that
     * treated null as "up to date" would say so about a Hub that never asked.
     *
     * @param {string | null} current
     * @param {string | null} latest
     * @returns {{latest: string} | null}
     */
    function updateState(current, latest) {
        if (!current || !latest) return null;
        // Behind only. A build ahead of the newest release is a pre-release or a
        // local build, and telling its operator to downgrade would be wrong.
        return vcmp(current, latest) < 0 ? {latest: latest} : null;
    }

    /**
     * The last view a light refresh can merge onto, or null when there is none:
     * undefined before the first read, the "loading" sentinel during it, an error
     * body, or a light body that never carried the rest. Null is the caller's
     * signal to read in full rather than light.
     *
     * @param {any} view
     * @returns {any}
     */
    function mergeableView(view) {
        return view && typeof view === 'object' && !view.error_code && Array.isArray(view.warnings)
            ? view
            : null;
    }

    /**
     * What the next re-read of an open row should cost.
     *
     *   full   the whole view: the only read that renders a panel, so it is what
     *          a row starts with and what it returns to once a minute
     *   light  the gauges alone, laid over the view already on screen
     *   probe  the gauges alone again, but to find out whether a row that failed
     *          can be read at all: an answer is followed by a full read
     *
     * @param {any} previous the view currently on screen, if any
     * @param {number} sinceFullMs since the last full read
     * @param {number} everyMs how often a full read is due
     * @returns {'full' | 'light' | 'probe'}
     */
    function refreshPlan(previous, sinceFullMs, everyMs) {
        if (mergeableView(previous)) return sinceFullMs >= everyMs ? 'full' : 'light';
        // Only a view that failed is worth probing. Everything else, nothing read
        // yet or a read in flight, starts over with the read that renders.
        return previous && typeof previous === 'object' && previous.error_code ? 'probe' : 'full';
    }

    /**
     * A light body laid over the last full view: the gauges and the version are
     * the light read's, the settings and the daemon's hints stay the full read's,
     * and the state is derived from both.
     *
     * @param {any} previous a view mergeableView returned
     * @param {any} light
     * @returns {any}
     */
    function mergeLight(previous, light) {
        return Object.assign({}, previous, {
            observed_at_ms: light.observed_at_ms,
            version: light.version,
            uptime_seconds: light.uptime_seconds,
            traces: light.traces,
            analysis_queue: light.analysis_queue,
            findings: light.findings,
            state: lightState(light, previous.warnings.length)
        });
    }

    /**
     * The daemon's closed set of incident kinds, as the incidents screen labels
     * them. Anything else was already folded to `other` by the Hub's parser.
     * @type {Record<string, string>}
     */
    const INCIDENT_KIND_LABEL = {
        oom_kill: 'OOM kill',
        memory_saturation: 'memory saturation',
        restart: 'restart',
        deploy: 'deploy',
        other: 'other'
    };

    /**
     * The engine's twelve finding types, worded as its own dashboard words them.
     * Kept as a display map and nothing else: `finding_type` is a stored column,
     * an index key and a public query parameter of `/api/findings`, so the raw
     * string has to survive everywhere but the cell a person reads.
     *
     * The fallback is load-bearing rather than defensive. The Hub accepts any
     * string on ingest, so a newer engine or another producer can send a
     * thirteenth type, and it belongs on screen as itself.
     * @type {Record<string, string>}
     */
    const FINDING_TYPE_LABEL = {
        n_plus_one_sql: 'N+1 SQL',
        n_plus_one_http: 'N+1 HTTP',
        n_plus_one_messaging: 'N+1 messaging',
        redundant_sql: 'Redundant SQL',
        redundant_http: 'Redundant HTTP',
        slow_sql: 'Slow SQL',
        slow_http: 'Slow HTTP',
        slow_messaging: 'Slow messaging',
        excessive_fanout: 'Excessive fanout',
        chatty_service: 'Chatty service',
        pool_saturation: 'Pool saturation',
        serialized_calls: 'Serialized calls'
    };

    /**
     * The daemon's own reading of `oldest_finding_ms`: at or below the window's
     * start the ring still reached the whole window, above it part of the window
     * had already been evicted when the incident was frozen, and absent means the
     * ring was empty. The Hub publishes the same verdict as `capture`, this is the
     * page's copy of the rule for a row it has not asked the Hub about.
     * @param {{oldest_finding_ms?: number | null, window_from_ms: number}} incident
     * @returns {'complete' | 'partial' | 'empty'}
     */
    function incidentCapture(incident) {
        if (incident.oldest_finding_ms == null) return 'empty';
        return incident.oldest_finding_ms <= incident.window_from_ms ? 'complete' : 'partial';
    }

    /**
     * Whether a frozen finding was already burning before the incident or fired
     * only after it. A finding is stamped when its trace is analysed, one TTL
     * after its last span, so a stamp past `at_ms` belongs to the replacement.
     * @param {{first_seen_ms: number}} finding
     * @param {{at_ms: number}} incident
     * @returns {'before' | 'after'}
     */
    function findingPhase(finding, incident) {
        return finding.first_seen_ms > incident.at_ms ? 'after' : 'before';
    }

    /**
     * What the last incidents read of one daemon came to, in the words the
     * incidents screen uses under its table. `ok` says nothing beyond the age,
     * which is already there.
     * @type {Record<string, string>}
     */
    const INCIDENT_READ_STATE = {
        absent: 'it publishes no incidents route',
        unauthorized: 'it refused the Hub\'s key',
        error: 'the read failed'
    };

    /**
     * How fresh the Hub's copy of one daemon's incidents is. The screen lists
     * one of these per daemon, so a quiet fleet reads differently from a stale
     * copy: an hour-old read of nothing is not the same answer as no read.
     * @param {{name: string, incidents_read_ms?: number | null, incidents_state?: string | null}} source
     * @param {number} nowMs
     * @returns {string}
     */
    function incidentsCopy(source, nowMs) {
        if (source.incidents_read_ms == null) return source.name + ': never read';
        const age = source.name + ': read ' + dur(Math.max(0, nowMs - source.incidents_read_ms)) + ' ago';
        const state = INCIDENT_READ_STATE[source.incidents_state || ''];
        return state ? age + ', ' + state : age;
    }

    /**
     * The route that opens New analysis on an incident's window. The end is held
     * at now: an incident younger than two TTLs still has a `window_to_ms` in the
     * future, and the Hub refuses a window that ends there.
     * @param {{id: string, service: string, window_from_ms: number, window_to_ms: number}} incident
     * @param {number} nowMs
     * @returns {string}
     */
    function incidentHandoffHash(incident, nowMs) {
        return '#/new?from=' + incident.window_from_ms
            + '&to=' + Math.min(incident.window_to_ms, nowMs)
            + '&service=' + encodeURIComponent(incident.service)
            + '&incident=' + encodeURIComponent(incident.id);
    }

    /**
     * What a `#/new?from=…&to=…&service=…` hash carries for the form, or null
     * when it carries nothing the form can take: both bounds numbers with the
     * start before the end, a service name, and the end no later than now, held
     * there rather than refused since a shared link ages. Any other route, and
     * the bare `#/new` the tab points at, read as null.
     * @param {string | null | undefined} hash
     * @param {number} nowMs
     * @returns {{fromMs: number, toMs: number, service: string, incidentId: string} | null}
     */
    function readHandoff(hash, nowMs) {
        const text = String(hash || '');
        if (text.indexOf('#/new?') !== 0) return null;
        const params = new URLSearchParams(text.slice('#/new?'.length));
        const number = function (key) {
            const raw = params.get(key);
            return raw === null || raw.trim() === '' ? NaN : Number(raw);
        };
        const fromMs = number('from');
        const toMs = Math.min(number('to'), nowMs);
        const service = (params.get('service') || '').trim();
        if (!Number.isFinite(fromMs) || !Number.isFinite(toMs) || fromMs >= toMs || service === '') return null;
        return {fromMs: fromMs, toMs: toMs, service: service, incidentId: params.get('incident') || ''};
    }

    /**
     * The route of the ack page. The environment and the source only decide
     * which rows start checked, so a link without them is whole. A blank one is
     * left out: Grafana sends a single space for the All choice of a variable.
     * Both travel trimmed, since a padded one would match no row.
     * @param {string} signature
     * @param {string | null | undefined} sourceId
     * @param {string | null | undefined} [environment]
     * @returns {string}
     */
    function ackRouteHash(signature, sourceId, environment) {
        const part = function (key, value) {
            const clean = String(value || '').trim();
            return clean === '' ? '' : '&' + key + '=' + encodeURIComponent(clean);
        };
        return '#/ack?signature=' + encodeURIComponent(signature)
            + part('environment', environment) + part('source_id', sourceId);
    }

    /**
     * The ack route for a `/?ack=<signature>&environment=…&source_id=…` address,
     * the last two optional, or null without an `ack` parameter. The link
     * arrives as a query because a hash is lost when the identity provider asks
     * for a password on the way in. An empty value still routes, so the page can
     * say the link is incomplete.
     * @param {string | null | undefined} search
     * @returns {string | null}
     */
    function ackEntryHash(search) {
        const params = new URLSearchParams(String(search || ''));
        return params.has('ack')
            ? ackRouteHash(params.get('ack') || '', params.get('source_id'), params.get('environment'))
            : null;
    }

    /**
     * What a `#/ack?signature=…` hash names, or null when the page cannot act
     * on it. The signature's bounds are the Hub's own, 1,024 characters and no
     * control character. A blank signature is refused here because
     * `/api/findings` reads one as no filter and would answer with a finding the
     * link never named. A blank source or environment is no context, read as
     * null, while an environment past 256 characters or carrying a control
     * character is a link nobody meant to write.
     * @param {string | null | undefined} hash
     * @returns {{signature: string, sourceId: string | null, environment: string | null} | null}
     */
    function readAckRoute(hash) {
        const text = String(hash || '');
        if (text.indexOf('#/ack?') !== 0) return null;
        const params = new URLSearchParams(text.slice('#/ack?'.length));
        const named = function (key) {
            return (params.get(key) || '').trim() || null;
        };
        const signature = params.get('signature') || '';
        if (signature.trim() === '' || signature.length > 1024 || /\p{Cc}/u.test(signature)) return null;
        const environment = named('environment');
        if (environment !== null && (environment.length > 256 || /\p{Cc}/u.test(environment))) return null;
        return {signature: signature, sourceId: named('source_id'), environment: environment};
    }

    /**
     * The ack reads whose listing the Hub mirrors acks from. Any other state
     * leaves `acks[]` silent about that source, which proves nothing.
     */
    const ACK_STATES_KNOWN = ['ok', 'truncated'];

    /**
     * One row per source that carries the finding, with what the page can do
     * there. An ack lives in one daemon's own store, so each source is acked and
     * revoked by itself, and an ack hides a finding, so the link's context sets
     * the default ticks: the source it names, else the sources of the environment
     * it names, else a source that is alone in taking an action. Every other row
     * stays the reader's to tick.
     *
     * A relaying source whose ack state is unknown takes `either` action. A
     * daemon below 0.24.0 never has a listing and one failed read proves
     * nothing, while the relay would still answer, so the daemon is left to
     * refuse the button that does not apply.
     * @param {{sources?: Array<{id: string, name: string, environment: string}>,
     *   acks?: Array<{source_id: string, source: string}>}} finding
     * @param {Array<import('../types').Source> | null | undefined} sources
     * @param {{sourceId?: string | null, environment?: string | null} | null | undefined} scope
     * @returns {Array<{id: string, name: string, environment: string, relay: boolean,
     *   ack: {source_id: string, source: string} | null, action: 'ack' | 'revoke' | 'either' | 'none',
     *   checked: boolean, note: string | null}>}
     */
    function ackRows(finding, sources, scope) {
        const sourceId = (scope && scope.sourceId) || null;
        const environment = (scope && scope.environment) || null;
        const rows = (finding.sources || []).map(function (carrier) {
            const source = (sources || []).find(function (candidate) {
                return candidate.id === carrier.id;
            });
            const ack = (finding.acks || []).find(function (candidate) {
                return candidate.source_id === carrier.id;
            }) || null;
            const relay = Boolean(source && source.ack_relay);
            let action = 'none';
            let note = null;
            if (!source) note = 'The Hub no longer configures this source.';
            else if (!relay) note = 'The Hub holds no ack credential for this source.';
            else if (ack && ack.source === 'daemon') action = 'revoke';
            else if (ack) note = 'Acknowledged by the CI baseline, which is edited through a pull request.';
            else if (ACK_STATES_KNOWN.indexOf(source.acks_state) < 0) {
                action = 'either';
                note = 'The ack state of this daemon is unknown, acks_state is '
                    + (source.acks_state || 'never read') + '. Acknowledge and Revoke both apply, the daemon '
                    + 'refuses the one that does not, and its answer shows in the result line.';
            } else action = 'ack';
            return {
                id: carrier.id,
                name: (source || carrier).name,
                environment: (source || carrier).environment,
                relay: relay,
                ack: ack,
                action: /** @type {'ack' | 'revoke' | 'either' | 'none'} */ (action),
                checked: false,
                note: note
            };
        });
        const actionable = rows.filter(function (row) {
            return row.action !== 'none';
        });
        actionable.forEach(function (row) {
            if (sourceId) row.checked = row.id === sourceId;
            else if (environment) row.checked = row.environment === environment;
            else row.checked = actionable.length === 1;
        });
        return rows;
    }

    /**
     * The `expires_at` of an ack that lasts through the day a date input holds:
     * the last second of that day in UTC, null for no date, which is a permanent
     * ack. A day already over in UTC throws, since the Hub refuses an expiry
     * that is not ahead, and the message is the sentence the page shows.
     * @param {string | null | undefined} dateValue
     * @param {number} nowMs
     * @returns {string | null}
     */
    function ackExpiry(dateValue, nowMs) {
        const day = String(dateValue || '');
        if (day === '') return null;
        const endMs = Date.parse(day + 'T23:59:59Z');
        // The engine rolls the 31st of February over rather than refusing it.
        if (!Number.isFinite(endMs) || isoUtc(endMs).slice(0, 10) !== day) {
            throw new RangeError('The expiry is not a date.');
        }
        if (endMs <= nowMs) throw new RangeError('The expiry is in the past.');
        return isoUtc(endMs);
    }

    /**
     * Whether a row is checked: the reader's own tick when they made one for the
     * action the row offers now, what ackRows says otherwise. A tick made on an
     * ack says nothing once the row reads Revoke, while a source the reader left
     * out stays out however often the rows are read again. `either` overlaps
     * both actions, so a tick made on it or read against it still speaks: an ack
     * state that turns known or unknown between two reads must not tick a row
     * the reader left out.
     * @param {{id: string, action: string, checked: boolean}} row
     * @param {Record<string, {action: string, checked: boolean}> | null | undefined} ticks
     * @returns {boolean}
     */
    function ackChecked(row, ticks) {
        if (row.action === 'none') return false;
        const own = (ticks || {})[row.id];
        const speaks = own && (own.action === row.action || own.action === 'either' || row.action === 'either');
        return speaks ? own.checked : row.checked;
    }

    /**
     * @param {number} count
     * @returns {string}
     */
    function sourceCount(count) {
        return count + (count === 1 ? ' source' : ' sources');
    }

    /**
     * What the two buttons would do with the form as it stands. `blocker` is
     * what keeps Acknowledge dead, while a revoke needs a checked row and nothing
     * else. `blocked` is true only when neither button can be pressed, so the
     * sentence never reads as a refusal beside a button that is ready. A checked
     * row that takes `either` action counts for both buttons.
     * @param {Array<{id: string, action: string, checked: boolean}>} rows
     * @param {Record<string, {action: string, checked: boolean}> | null | undefined} ticks
     * @param {string | null | undefined} reason
     * @param {string | null | undefined} expiryValue
     * @param {number} nowMs
     * @returns {{ack: Array<{id: string, action: string, checked: boolean}>,
     *   revoke: Array<{id: string, action: string, checked: boolean}>, expiresAt: string | null,
     *   blocker: string | null, blocked: boolean, sentence: string}}
     */
    function ackPlan(rows, ticks, reason, expiryValue, nowMs) {
        const checked = function (action) {
            return rows.filter(function (row) {
                return (row.action === action || row.action === 'either') && ackChecked(row, ticks);
            });
        };
        const ack = checked('ack');
        const revoke = checked('revoke');
        let expiresAt = null;
        let blocker = null;
        if (ack.length === 0) blocker = 'No checked source can take an ack.';
        else if (String(reason || '').trim() === '') blocker = 'An ack needs a reason.';
        else {
            try {
                expiresAt = ackExpiry(expiryValue, nowMs);
            } catch (error) {
                blocker = String(error.message);
            }
        }
        const sentences = [];
        if (ack.length > 0) {
            sentences.push(blocker || 'Acknowledge writes to ' + sourceCount(ack.length)
                + (expiresAt ? ', until ' + expiresAt + '.' : ', with no expiry.'));
        }
        // The Hub mirrors no ack from a row of unknown state, so it claims none there.
        const unsure = revoke.some(function (row) {
            return row.action === 'either';
        });
        if (revoke.length > 0) {
            sentences.push(unsure
                ? 'Revoke asks ' + sourceCount(revoke.length) + ' to remove the ack, if there is one.'
                : 'Revoke removes the ack on ' + sourceCount(revoke.length) + '.');
        }
        return {
            ack: ack,
            revoke: revoke,
            expiresAt: expiresAt,
            blocker: blocker,
            blocked: blocker !== null && revoke.length === 0,
            sentence: sentences.join(' ') || 'No checked source can take an ack or a revoke.'
        };
    }

    /**
     * The JSON one relay request carries. The Hub names the caller itself, and a
     * revoke names the finding and nothing else.
     * @param {'ack' | 'revoke'} action
     * @param {string} signature
     * @param {string | null | undefined} reason
     * @param {string | null | undefined} expiresAt
     * @returns {{signature: string, reason?: string, expires_at?: string}}
     */
    function ackBody(action, signature, reason, expiresAt) {
        if (action === 'revoke') return {signature: signature};
        /** @type {{signature: string, reason: string, expires_at?: string}} */
        const body = {signature: signature, reason: String(reason || '').trim()};
        if (expiresAt) body.expires_at = expiresAt;
        return body;
    }

    /**
     * The refusals the Hub sends with no body, plus 0 for a request that never
     * got an answer. Every other refusal carries its own `detail`.
     * @type {Record<number, string>}
     */
    const ACK_REFUSALS = {
        0: 'The Hub did not answer.',
        413: 'The request is larger than the Hub accepts.',
        503: 'The Hub is already relaying two acks. Try again in a moment.'
    };

    /**
     * One line per source a submit wrote to, in the order it wrote. `action` is
     * the button pressed, never `either`: on a row of unknown ack state the
     * relay's `detail` carries the daemon's refusal of the button that did not apply.
     * @param {Array<{name: string, action: 'ack' | 'revoke', status: number, detail?: string | null}>} results
     * @returns {Array<{ok: boolean, text: string}>}
     */
    function ackSummary(results) {
        return results.map(function (result) {
            const ok = result.status >= 200 && result.status < 300;
            const said = ok
                ? (result.action === 'revoke' ? 'ack revoked.' : 'acknowledged.')
                : result.detail || ACK_REFUSALS[result.status]
                || 'The Hub refused the request with status ' + result.status + '.';
            return {ok: ok, text: result.name + ': ' + said};
        });
    }

    global.PSL = {
        setVersions,
        get ENGINE() {
            return ENGINE;
        },
        get HUB() {
            return HUB;
        },
        ERRORS, READ_ERRORS, ERROR_TITLES, KIND_LABEL,
        dur, durPrecise, durMinutes, durParts, splitByKind, clock, parseDur, humanDur, dtLocal, dtHuman, bytes,
        vparts, vcmp, versionGap, skew, detector, statusKey, argsLine, weightBand,
        shq, psq, SHELLS, shellById, defaultShell, exportLine,
        analysisCommand, monitorCommand, detectionToml, quotedForShell,
        lightState, mergeableView, mergeLight, refreshPlan, releaseUrl, openFolds,
        hubReleaseUrl, updateState, knownShell, CHART_PAGE, CHART_COORDINATE,
        gaugeTone, gaugeMove,
        INCIDENT_KIND_LABEL, FINDING_TYPE_LABEL, incidentCapture, findingPhase, INCIDENT_READ_STATE, incidentsCopy,
        incidentHandoffHash, readHandoff,
        ackRouteHash, ackEntryHash, readAckRoute, ackRows, ackExpiry, ackChecked, ackPlan, ackBody, ackSummary
    };
})(globalThis);
