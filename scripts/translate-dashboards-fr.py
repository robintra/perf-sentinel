#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Generate the French variants of the two example Grafana dashboards.

examples/FR/grafana-dashboard-FR.json and examples/FR/grafana-findings-dashboard-FR.json
are examples/grafana-dashboard.json and examples/grafana-findings-dashboard.json
with every visible string translated: titles, descriptions, variable labels,
prose legends, value mappings and column headers. Queries, layout, units,
thresholds, colors, uid and version stay those of the English file, so the two
can only differ in wording. Edit the English file, then regenerate:

    python3 scripts/translate-dashboards-fr.py           # write examples/FR/
    python3 scripts/translate-dashboards-fr.py --check   # fail if examples/FR/ is stale

Both modes fail when the English file holds a string the table below does not
know (a new panel, a reworded description): add the entry rather than forcing
it through. Stdlib only.

PromQL, metric and label names, and snake_case legends stay in English on
purpose. So do shed_batches/s, archive_drops/s and pair_evictions/s: a
fieldConfig override on the headroom panel matches those three byName, and
renaming one moves its series to the left percent axis with no error anywhere.
The checks at the bottom of this file exist for exactly that.
"""

import io
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXAMPLES = os.path.join(ROOT, "examples")


TABLE = []       # Prometheus dashboard, grafana-dashboard.json
FINDINGS = []    # query API dashboard, grafana-findings-dashboard.json


def t(field, en, fr):
    TABLE.append((field, en, fr))


def f(field, en, fr):
    FINDINGS.append((field, en, fr))


t("title", "I/O ops rate by service", "Débit d'opérations I/O par service")
t("title", "Slow span duration p95 by kind", "p95 de durée des spans lents par nature")
t("title", "Top 10 finding types by severity", "Top 10 des types de findings par sévérité")
t("title", "Slow span duration distribution", "Distribution des durées de spans lents")
t("title", "I/O waste ratio", "Ratio d'I/O gaspillées")
t("title", "Critical findings", "Findings critiques")
t("title", "Findings distribution by type", "Répartition des findings par type")
t("title", "Total findings (cumulative)", "Total des findings (cumulé)")
t("title", "Ingested I/O ops (cumulative)", "Opérations I/O ingérées (cumulé)")
t("title", "Avoidable I/O ops rate", "Débit d'opérations I/O évitables")
t("title", "Findings detail by type and severity", "Détail des findings par type et sévérité")
t("title", "Daemon health (global)", "Santé du daemon (global)")
t("title", "Daemon health", "Santé du daemon")
t("title", "Traces analyzed (cumulative)", "Traces analysées (cumulé)")
t("title", "Active traces", "Traces actives")
t("title", "Events processed rate", "Débit d'événements traités")
t("title", "Energy ingestion freshness", "Fraîcheur des relevés d'énergie")
t("title", "Runtime headroom and shedding", "Marge d'exécution et délestage")
t("title", "OTLP span intake (received vs filtered)", "Admission des spans OTLP (reçus vs filtrés)")
t("title", "Report exports rate", "Débit d'exports de rapport")
t("title", "Energy per scoring window (kWh)", "Énergie par fenêtre de scoring (kWh)")
t("title", "Carbon per scoring window (gCO2e)", "Carbone par fenêtre de scoring (gCO2e)")

# --- template variable labels ---
t("label", "Daemon namespace", "Namespace du daemon")

# --- panel and variable descriptions ---
t("description",
  "The Prometheus instance scraping the perf-sentinel daemon /metrics endpoint.",
  "L'instance Prometheus qui scrape l'endpoint /metrics du daemon perf-sentinel.")

t("description",
  "Database queries and outbound HTTP calls made by each instrumented service, per second: the raw workload, before any judgement about whether the calls were needed.\n\nA service that is loud here and also appears in the findings panels is where optimisation pays most.",
  "Requêtes base de données et appels HTTP sortants de chaque service instrumenté, par seconde : la charge brute, avant tout jugement sur la nécessité des appels.\n\nUn service bruyant ici et aussi présent dans les panneaux de findings est celui où l'optimisation rapporte le plus.")

t("description",
  "95th percentile duration of spans that crossed the slow threshold (500 ms by default, configurable), split by kind: SQL, outbound HTTP and messaging.\n\nOnly slow spans enter this histogram, so the value is always above the threshold by construction and is not your overall latency.",
  "95e centile de la durée des spans ayant dépassé le seuil de lenteur (500 ms par défaut, configurable), séparé par nature : SQL, HTTP sortant et messaging.\n\nSeuls les spans lents entrent dans cet histogramme, donc la valeur est toujours au-dessus du seuil par construction et n'est pas la latence globale.")

t("description",
  "A finding is one detected inefficiency, for example an N+1 loop (the same query repeated once per row instead of a single batched one). This ranks the most frequent types by severity over the selected time range.\n\nRising counts mean new traffic reaching an unoptimised path, or a recent deploy that regressed.",
  "Un finding est une inefficacité détectée, par exemple une boucle N+1 (la même requête répétée une fois par ligne au lieu d'une seule requête groupée). Ce panneau classe les types les plus fréquents par sévérité sur la plage de temps sélectionnée.\n\nDes comptes qui montent signalent du trafic nouveau sur un chemin non optimisé, ou une livraison récente qui a régressé.")

t("description",
  "Heatmap of slow span buckets over time, complementing the global p95 panel with the full duration distribution. Hot bands above the p95 line indicate tail latency drift.",
  "Heatmap des tranches de spans lents dans le temps, en complément du panneau p95 global avec la distribution complète des durées. Des bandes chaudes au-dessus de la ligne p95 signalent une dérive de la latence de queue.")



t("description",
  "The same findings as the ranking panel, shown as a share of the whole. A single type dominating usually points at one framework or one query pattern rather than at a broad problem.\n\nA type missing from the legend had no increase over the range, which is not the same as never emitted.",
  "Les mêmes findings que le panneau de classement, vus en part du total. Un seul type qui domine pointe en général vers un framework ou un motif de requête, pas vers un problème général.\n\nUn type absent de la légende n'a pas augmenté sur la plage, ce qui n'est pas la même chose que jamais émis.")

t("description",
  "Findings emitted since daemon start for the selected services (the grand total with All selected), summed across all types and severities. Pairs with the per-type / per-severity panels to give a single at-a-glance number.",
  "Findings émis depuis le démarrage du daemon pour les services sélectionnés (le total général avec All), tous types et toutes sévérités confondus. Complète les panneaux par type et par sévérité avec un seul chiffre d'ensemble.")


t("description",
  "Operations per second the analyzer marked as avoidable, meaning the same result could have been obtained with fewer calls, read from `perf_sentinel_service_avoidable_io_ops_total`. Unlike the cumulative waste ratio it reacts immediately, so an optimisation PR landing shows here.\n\nFed by N+1 SQL, N+1 HTTP, N+1 messaging, Redundant SQL and Redundant HTTP, one avoidable operation per repeat beyond the first call.\n\nA rise under a flat findings rate means an existing pattern grew, more repeats per trace, not that a new one appeared.",
  "Opérations par seconde que l'analyseur a marquées comme évitables, c'est-à-dire que le même résultat aurait pu être obtenu avec moins d'appels, lues sur `perf_sentinel_service_avoidable_io_ops_total`. Contrairement au ratio de gaspillage cumulé, ce panneau réagit tout de suite, donc l'arrivée d'une PR d'optimisation se voit ici.\n\nAlimenté par N+1 SQL, N+1 HTTP, N+1 messaging, SQL redondant et HTTP redondant, une opération évitable par répétition au-delà du premier appel.\n\nUne hausse avec un débit de findings plat veut dire qu'un motif existant a grossi, plus de répétitions par trace, pas qu'un nouveau est apparu.")

t("description",
  "Finding counts by type and severity over the selected time range, read from `perf_sentinel_findings_total`, the same window as the ranking and distribution panels beside it, so the three never contradict each other.\n\nUseful for raw triage and pasting into tickets. The cumulative total since daemon start is the 'Total findings (cumulative)' stat.",
  "Décompte des findings par type et sévérité sur la plage de temps sélectionnée, lu sur `perf_sentinel_findings_total`, la même fenêtre que les panneaux de classement et de répartition à côté, donc les trois ne se contredisent jamais.\n\nUtile pour le tri brut et le copier-coller dans un ticket. Le total cumulé depuis le démarrage du daemon est dans la statistique 'Total des findings (cumulé)'.")

t("description",
  "up{} for the perf-sentinel daemon scrape job. 0 means Prometheus cannot reach /metrics. Adjust the job label to match your ServiceMonitor / scrape_configs naming.",
  "up{} du job de scrape du daemon perf-sentinel. 0 signifie que Prometheus n'atteint pas /metrics. Ajuster le label job pour coller au nommage du ServiceMonitor ou des scrape_configs.")

t("description",
  "Traces that completed analysis since daemon start. Trace-level cadence, where 'events processed' is span-level. The ratio between the two gives the average trace size, which is a useful sanity check on your instrumentation coverage.",
  "Traces dont l'analyse s'est terminée depuis le démarrage du daemon. Cadence au niveau trace, là où le débit d'événements traités est au niveau span. Le rapport entre les deux donne la taille moyenne d'une trace, un bon contrôle de la couverture d'instrumentation.")

t("description",
  "Traces currently held in memory waiting to be completed, from `perf_sentinel_active_traces`. A trace is released for analysis once it expires (30s by default) or the window is full.\n\nA stable plateau is healthy. Continuous growth means traces arrive faster than they expire and the cap will start dropping the oldest ones, see the headroom panel.",
  "Traces actuellement gardées en mémoire en attente d'être complètes, d'après `perf_sentinel_active_traces`. Une trace part à l'analyse quand elle expire (30 s par défaut) ou quand la fenêtre est pleine.\n\nUn plateau stable est sain. Une croissance continue veut dire que les traces arrivent plus vite qu'elles n'expirent et que le plafond va commencer à jeter les plus anciennes, voir le panneau de marge.")

t("description",
  "Spans accepted into the analysis pipeline per second, from `perf_sentinel_events_processed_total`, after non-I/O spans have been filtered out.\n\nCompare with the OTLP intake panel: a large gap is normal, most spans in a trace are not I/O. A drop to zero while intake continues means the pipeline stopped consuming.",
  "Spans admis dans le pipeline d'analyse par seconde, d'après `perf_sentinel_events_processed_total`, une fois les spans sans I/O filtrés.\n\nÀ comparer au panneau d'admission OTLP : un écart important est normal, la plupart des spans d'une trace ne sont pas des I/O. Une chute à zéro alors que l'admission continue veut dire que le pipeline ne consomme plus.")

t("description",
  "Seconds since each configured energy backend last returned a sample. Series are gated on `perf_sentinel_energy_backend_configured`, so a backend you did not configure draws nothing.\n\nZero means fresh. An age past the scrape interval means stale readings, and carbon attribution silently falls back to the built-in estimate while the numbers stay plausible.",
  "Secondes écoulées depuis le dernier échantillon renvoyé par chaque backend d'énergie configuré. Les séries sont conditionnées à `perf_sentinel_energy_backend_configured`, donc un backend non configuré ne dessine rien.\n\nZéro veut dire frais. Un âge qui dépasse l'intervalle de relevé veut dire que les mesures sont périmées, et l'attribution carbone retombe silencieusement sur l'estimation interne alors que les chiffres restent plausibles.")



t("description",
  "rate(perf_sentinel_export_report_requests_total). Tracks how often /api/export/report is consumed (CI snapshots, dev triage, daemon-Report to local-batch path).",
  "rate(perf_sentinel_export_report_requests_total). Suit la fréquence d'appel de /api/export/report (instantanés de CI, tri en dev, passage du rapport du daemon vers une analyse locale).")

t("description",
  "Electricity attributed to the analysed workload for the most recent scoring window. Depending on configuration it is measured (an agent reading real power) or modelled from operation counts, the energy source line of `query monitor` and the HTML report says which.\n\nPer-service and per-region detail is kept off /metrics and lives in the query monitor.",
  "Électricité attribuée à la charge analysée pour la dernière fenêtre de scoring. Selon la configuration, elle est mesurée (un agent qui lit la puissance réelle) ou modélisée à partir du nombre d'opérations, et la ligne de source d'énergie de `query monitor` et du rapport HTML indique lequel des deux.\n\nLe détail par service et par région est tenu hors de /metrics et se trouve dans le monitor de requêtes.")





# --- text shown when a panel has no series ---
t("noValue",
  "No measured-energy backend configured. Carbon figures come from the built-in I/O proxy. See docs/ENERGY.md to plug in Alumet, Scaphandre, Kepler, Redfish or cloud SPECpower.",
  "Aucun backend de mesure d'énergie configuré. Les chiffres carbone viennent du proxy I/O interne. Voir docs/ENERGY.md pour brancher Alumet, Scaphandre, Kepler, Redfish ou le SPECpower cloud.")

# --- prose legends. snake_case legends and the three an override matches
# byName are deliberately absent from this table, see the module docstring ---
t("legendFormat", "received/s", "reçus/s")
t("legendFormat", "rejected/s {{reason}}", "rejetés/s {{reason}}")
t("legendFormat", "avoidable ops/s", "ops évitables/s")
t("legendFormat", "ingested ops", "ops ingérées")
t("legendFormat", "waste ratio", "ratio de gaspillage")
t("legendFormat", "events/s {{instance}}", "événements/s {{instance}}")
t("legendFormat", "energy {{instance}}", "énergie {{instance}}")
t("legendFormat", "carbon {{instance}}", "carbone {{instance}}")

# --- 0.19.0: grouping label, and the panels added upstream after it ---
t("title", "Analysis (filtered by grouping and service)", "Analyse (filtrée par namespace des services et par service)")
t("title", "Findings rate by type", "Débit de findings par type")
t("title", "Ingest memory pressure", "Pression mémoire à l'ingestion")
t("title", "Daemon memory (RSS)", "Mémoire du daemon (RSS)")
t("title", "Energy scrape outcome", "Résultat des relevés d'énergie")
t("title", "Cardinality caps (overflow)", "Plafonds de cardinalité (débordement)")
t("title", "Hub export (pending and dropped)", "Export vers le Hub (en attente et perdus)")

# The variable, the label and the PromQL value stay `grouping`; only what a
# reader sees is translated.
t("label", "Grouping", "Namespace des services")

t("legendFormat", "filtered/s {{reason}}", "filtrés/s {{reason}}")
t("legendFormat", "pending {{instance}}", "en attente {{instance}}")
t("legendFormat", "ingest I/O ops, 1024 services (left out)", "ops I/O ingestion, 1024 services (écartés)")
t("legendFormat", "ingest I/O ops, 4096 pairs", "ops I/O ingestion, 4096 paires")
t("legendFormat", "analysis, 128 services", "analyse, 128 services")
t("legendFormat", "analysis, 512 pairs", "analyse, 512 paires")
t("legendFormat", "slow histogram, 64 services", "histogramme lents, 64 services")
t("legendFormat", "slow histogram, 256 pairs", "histogramme lents, 256 paires")

t("noValue",
  "No process metrics: the process collector is registered on Linux only.",
  "Aucune métrique de processus : le collecteur de processus n'est enregistré que sur Linux.")

t("noValue",
  "No measured-energy backend configured, or none scraped yet.",
  "Aucun backend de mesure d'énergie configuré, ou aucun relevé pour l'instant.")

t("description",
  "Share of I/O operations attributed to avoidable anti-patterns over the selected time range: `perf_sentinel_service_avoidable_io_ops_total` divided by `perf_sentinel_service_analyzed_io_ops_total`.\n\nServices past the 128-service cap fold into service=\"_other\", (service, grouping) pairs past the 512-pair cap into grouping=\"_other\", and both counters are silent when green scoring is off.\n\nThe numerator counts repeats beyond the first call for N+1 SQL, N+1 HTTP, N+1 messaging, Redundant SQL and Redundant HTTP. Slow spans, Chatty service, Excessive fanout, Serialized calls and Pool saturation are necessary operations and add nothing.",
  "Part des opérations I/O attribuées à des anti-motifs évitables sur la plage de temps sélectionnée : `perf_sentinel_service_avoidable_io_ops_total` divisé par `perf_sentinel_service_analyzed_io_ops_total`.\n\nLes services au-delà du plafond de 128 services se replient sur service=\"_other\", les paires (service, grouping) au-delà du plafond de 512 paires se replient sur grouping=\"_other\", et les deux compteurs sont muets si le scoring green est coupé.\n\nLe numérateur compte les répétitions au-delà du premier appel pour N+1 SQL, N+1 HTTP, N+1 messaging, SQL redondant et HTTP redondant. Les spans lents, Service bavard, Fanout excessif, Appels sérialisés et Saturation de pool sont des opérations nécessaires et n'y ajoutent rien.")

t("description",
  "Findings rated critical over the selected time range. Severity measures how far the pattern exceeds its detection threshold, not user impact: a strong optimisation candidate, not an outage.\n\nShows 0 rather than 'no data' when none were emitted, so a flat zero means healthy, unless the selected service was folded past the 128-service analysis cap into service=\"_other\", or its (service, grouping) pair past the 512-pair cap into grouping=\"_other\".",
  "Findings classés critiques sur la plage de temps sélectionnée. La sévérité mesure l'écart au seuil de détection, pas l'impact utilisateur : un bon candidat à l'optimisation, pas une panne.\n\nAffiche 0 plutôt que 'no data' quand aucun n'a été émis, donc un zéro constant veut dire sain, sauf si le service sélectionné, au-delà du plafond d'analyse de 128 services, a été replié sur service=\"_other\", ou si sa paire (service, grouping), au-delà du plafond de 512 paires, a été repliée sur grouping=\"_other\".")

t("description",
  "Ingested I/O operations since the daemon started, per service, counted at ingest admission: an analysis stall does not flatten it, watch `perf_sentinel_analysis_queue_depth` and the shed counters for that.\n\nOps past the 1024-service ingest cap are dropped, not folded, and (service, grouping) pairs past the 4096-pair cap fold into grouping=\"_other\". Resets on restart. Flat while traffic continues means ingestion has stalled.",
  "Opérations I/O ingérées depuis le démarrage du daemon, par service, comptées à l'admission : un blocage de l'analyse n'aplatit pas la série, surveiller `perf_sentinel_analysis_queue_depth` et les compteurs de délestage pour cela.\n\nLes opérations au-delà du plafond d'ingestion de 1024 services sont jetées, pas repliées, et les paires (service, grouping) au-delà du plafond de 4096 paires se replient sur grouping=\"_other\". Remis à zéro au redémarrage. Une série plate alors que le trafic continue veut dire que l'ingestion est bloquée.")

t("description",
  "Findings emitted per second by type, for the selected grouping and services, read from `perf_sentinel_findings_total`. The same counter the ranking, the pie and the table read over the whole range, shown here as a trend, so a deploy that regressed or fixed a path shows as a step.\n\nThe only panel with exemplars enabled. With exemplar storage on Prometheus and a trace datasource mapped in Grafana (docs/HELM-DEPLOYMENT.md, Exemplars), each series carries diamonds linking to the worst trace of its type, severity, service and grouping, kept 15 minutes after the batch that recorded it. Without that setup the diamonds are absent and the counts are unchanged.",
  "Findings émis par seconde par type, pour le namespace des services et les services sélectionnés, lus sur `perf_sentinel_findings_total`. Le même compteur que lisent le classement, la répartition et le tableau sur toute la plage, vu ici en tendance, donc une livraison qui a dégradé ou corrigé un chemin apparaît comme une marche.\n\nSeul panneau avec les exemplars activés. Avec le stockage des exemplars côté Prometheus et un datasource de traces associé dans Grafana (docs/HELM-DEPLOYMENT.md, Exemplars), chaque série porte des losanges qui pointent vers la pire trace de son type, de sa sévérité, de son service et de son namespace, gardée 15 minutes après le lot qui l'a enregistrée. Sans cette configuration, les losanges sont absents et les comptes sont inchangés.")

t("description",
  "`perf_sentinel_ingest_memory_pressure`, 1 while the cgroup admission guard refuses ingest (HTTP 503, gRPC UNAVAILABLE, both retryable) to keep RSS under the memory limit. `PerfSentinelMemoryPressureRejecting` fires after 5 minutes of it.\n\nRefused requests count on the OTLP intake panel under reason=\"memory_pressure\", as requests rather than spans.\n\nAlso 0 when the guard is off (`[daemon] memory_high_water_pct = 0`, the default) or without a cgroup v2 memory limit, so OK does not mean the guard is armed, `/api/config` does. The area behind the value is the state over the selected range.",
  "`perf_sentinel_ingest_memory_pressure`, à 1 tant que le garde-fou d'admission cgroup refuse l'ingestion (HTTP 503, gRPC UNAVAILABLE, tous deux rejouables) pour tenir la RSS sous la limite mémoire. `PerfSentinelMemoryPressureRejecting` se déclenche au bout de 5 minutes dans cet état.\n\nLes requêtes refusées sont comptées sur le panneau d'admission OTLP sous reason=\"memory_pressure\", en requêtes et non en spans.\n\nVaut 0 aussi quand le garde-fou est coupé (`[daemon] memory_high_water_pct = 0`, la valeur par défaut) ou sans limite mémoire cgroup v2 : OK ne dit donc pas que le garde-fou est armé, `/api/config` le dit. La zone derrière la valeur est l'état sur la plage sélectionnée.")

t("description",
  "`process_resident_memory_bytes` per replica, from the standard process collector. It is registered on Linux only, so an empty panel on macOS or Windows is expected.\n\nThe memory-pressure guard compares the cgroup working set to `[daemon] memory_high_water_pct`, not this figure (they differ by the page cache). A climb that never plateaus is what the guard stops, and the plateau against `resources.limits.memory` is the number to size the limit from.",
  "`process_resident_memory_bytes` par réplica, issu du collecteur de processus standard. Il n'est enregistré que sur Linux : un panneau vide sur macOS ou Windows est donc normal.\n\nLe garde-fou de pression mémoire compare le working set du cgroup à `[daemon] memory_high_water_pct`, et non à ce chiffre (l'écart entre les deux est le cache de pages). Une montée qui ne se stabilise jamais est ce que le garde-fou arrête, et le niveau du plateau, face à `resources.limits.memory`, est le chiffre qui sert à dimensionner la limite.")

t("description",
  "Scrape attempts per second per configured measured-energy backend, by outcome, gated on `perf_sentinel_energy_backend_configured`. cloud_energy has no outcome counters and is on the freshness panel only.\n\nIt shows what the freshness gauge hides: a backend never scraped yet has no success series, one failing every other tick has a failed series beside it. Reasons: `perf_sentinel_<backend>_scrape_failed_total{reason}`.\n\nAn HTTP 200 with no matching sample still counts as a success while carbon attribution has fallen back to the built-in estimate. Only the daemon's warn log and the energy source line of `query monitor` and the HTML report show it, where the affected services count as modelled from I/O counts.",
  "Tentatives de relevé par seconde sur chaque backend de mesure d'énergie configuré, par résultat, conditionnées à `perf_sentinel_energy_backend_configured`. cloud_energy n'a pas de compteurs de résultat et n'apparaît que sur le panneau de fraîcheur.\n\nCe panneau montre ce que la jauge de fraîcheur cache : un backend encore jamais relevé n'a aucune série de succès, un backend qui échoue un tick sur deux a une série d'échecs à côté. Motifs : `perf_sentinel_<backend>_scrape_failed_total{reason}`.\n\nUne réponse HTTP 200 sans aucun échantillon correspondant compte quand même comme un succès, alors que l'attribution carbone est retombée sur l'estimation interne. Seuls le log warn du daemon et la ligne de source d'énergie de `query monitor` et du rapport HTML le montrent, et les services concernés y comptent comme modélisés à partir du nombre d'opérations I/O.")

t("description",
  "Left axis: active traces, analysis queue and findings store as a percentage of their caps, advisor hint at 90% (red line).\n\nRight axis, per second: shed batches and their traces, dropped archive windows, pair evictions. Shed traces are never analysed, `PerfSentinelAnalysisShedding` fires past one per second over ten minutes, when sustained raise `analysis_queue_capacity` or add CPU. Dropped archive windows were served live but the disclosure archive under-reports them.\n\nPair evictions are the correlator recycling pairs at `max_tracked_pairs`, after which `/api/correlations` silently returns an arbitrary subset. No alert covers it.",
  "Axe de gauche : traces actives, file d'analyse et findings stockés en pourcentage de leur plafond. Le conseiller le signale à partir de 90 % (ligne rouge).\n\nAxe de droite, par seconde : les lots délestés et leurs traces, les fenêtres d'archive jetées, les évictions de paires. Les traces délestées ne sont jamais analysées. `PerfSentinelAnalysisShedding` se déclenche au-delà d'une trace par seconde sur dix minutes, et un délestage durable veut dire qu'il faut monter `analysis_queue_capacity` ou ajouter du CPU. Les fenêtres d'archive jetées ont été servies en direct, mais l'archive de publication les sous-déclare.\n\nLes évictions de paires sont le corrélateur qui recycle ses paires au plafond `max_tracked_pairs`, à partir de quoi `/api/correlations` renvoie un sous-ensemble arbitraire sans que rien ne le signale. Aucune alerte ne le couvre.")

t("description",
  "Attributions per second turned away by a per-run cardinality cap. Five of the six fold into `service=\"_other\"` or `grouping=\"_other\"`, so totals stay exact and only newly seen services or groupings coarsen. The ingest I/O 1024-service cap leaves the service out instead, undercounting its throughput and measured-energy attribution.\n\nA flat zero is normal and nothing here alerts. A climb means more services or groupings than the cap, the Service and Grouping variables show the folded ones under `_other`.\n\nCaps hold until the daemon restarts, so a burst of one-off service names keeps folding every service that came after.",
  "Attributions par seconde qu'un plafond de cardinalité par exécution a écartées. Cinq des six se replient sur `service=\"_other\"` ou `grouping=\"_other\"`, donc les totaux restent exacts et seule l'attribution des services ou namespaces vus ensuite devient plus grossière. Le plafond de 1024 services du compteur I/O d'ingestion laisse plutôt le service de côté, et sous-compte ainsi son débit et son attribution d'énergie mesurée.\n\nUne ligne plate à zéro est l'état normal et rien ici ne déclenche d'alerte. Une série qui monte veut dire qu'il y a plus de services ou de namespaces que le plafond n'en accepte, et les variables Service et Namespace des services montrent sous `_other` ceux qui sont repliés.\n\nUne fois atteints, les plafonds le restent jusqu'au redémarrage du daemon, donc après une rafale de noms de services jetables, tous les services arrivés ensuite restent repliés.")

t("description",
  "Span intake at the OTLP boundary: received, filtered and rejected per second, the last two by reason.\n\n`filtered` spans carry no analyzable I/O operation. `not_io` is normal and usually the majority, `non_sql_datastore` and `merged_db_span` are deliberate. `missing_db_statement` and `missing_http_url` are instrumentation gaps that turn every request into zero events while every shipped alert stays green.\n\n`rejected`: `channel_full` is lost work and the only reason `PerfSentinelIngestRejecting` fires on, `memory_pressure` the admission guard, `parse_error` and `unsupported_media_type` malformed requests to fix in the producer.",
  "Admission des spans à la frontière OTLP : reçus, filtrés et rejetés par seconde, les deux derniers séparés par raison.\n\n`filtered` désigne les spans sans opération d'I/O analysable. `not_io` est normal et représente en général la majorité, `non_sql_datastore` et `merged_db_span` sont volontaires. `missing_db_statement` et `missing_http_url` sont des trous d'instrumentation qui transforment chaque requête en zéro événement pendant que toutes les alertes livrées restent au vert.\n\n`rejected` : `channel_full` est du travail perdu et la seule raison qui déclenche `PerfSentinelIngestRejecting`, `memory_pressure` est le garde-fou d'admission, `parse_error` et `unsupported_media_type` sont des requêtes malformées à corriger chez le producteur.")

t("description",
  "The bounded Hub export buffer, for installs with `[daemon.hub_export]` configured, flat at zero otherwise.\n\nPending is distinct finding signatures awaiting the next push, so it plateaus at the fleet's distinct-problem count and climbs past it only when pushes fail. Dropped (right axis) is evictions at `hub_export.max_pending`, findings too large for the payload and batches refused with an unretryable 4xx. Nothing replays them, so a nonzero rate means the Hub's view of this daemon is short.\n\nNo shipped alert, add `rate(perf_sentinel_hub_export_dropped_total[15m]) > 0` through `prometheusRule.additionalRules` if you run the Hub.",
  "Le tampon borné d'export vers le Hub, pour les installations où `[daemon.hub_export]` est configuré, les séries restant plates à zéro sinon.\n\nEn attente compte les signatures de findings distinctes qui attendent la prochaine poussée : la série plafonne donc au nombre de problèmes distincts du parc et ne monte au-delà que si les poussées échouent. Perdus (axe de droite) compte les évictions à `hub_export.max_pending`, les findings trop gros pour la charge utile et les lots refusés avec un 4xx non rejouable. Rien ne les rejoue, donc un débit non nul veut dire que la vue du Hub sur ce daemon est incomplète.\n\nAucune alerte livrée : ajouter `rate(perf_sentinel_hub_export_dropped_total[15m]) > 0` via `prometheusRule.additionalRules` si le Hub est en service.")

t("description",
  "The Prometheus job scraping perf-sentinel, one per install when several daemons share a Prometheus. All reads the fleet, pick one to keep staging apart from production. No .* fallback here, unlike Namespace: a scrape always sets job, so All expands to the jobs that carry perf-sentinel metrics and never to every target Prometheus knows.",
  "Le job Prometheus qui scrape perf-sentinel, un par installation quand plusieurs daemons partagent un Prometheus. All couvre tout le parc, en choisir un pour séparer la recette de la production. Pas de repli .* ici, contrairement à Namespace : un scrape pose toujours job, donc All se développe en les jobs qui portent des métriques perf-sentinel et jamais en toutes les cibles connues de Prometheus.")

t("description",
  "Namespace the daemon install runs in (attached by the scrape), not a namespace of the analysed services. For the analysed workloads' namespace use Grouping.",
  "Namespace où tourne l'installation du daemon (attaché par le scrape), pas un namespace des services analysés. Pour le namespace des charges analysées, utiliser Namespace des services.")

t("description",
  "Grouping of the analysed traffic: the first attribute present from [detection] grouping_attributes (k8s.namespace.name, then service.namespace, by default), so on Kubernetes the namespace the workloads run in. Filters the Analysis row and narrows the Service list. The value alone is the label: two attributes with the same value share one entry. grouping=\"_other\" is the fold bucket for (service, grouping) pairs past the per-run pair caps (512 on findings and the analysis-side I/O counters, 256 on the slow-span histogram, 4096 on the ingest counter); a folded pair keeps its service. A span that carried none of the configured attributes has an empty value, which Prometheus stores as no label at all: those series are only visible with All selected.",
  "Namespace du trafic analysé : le premier attribut présent parmi [detection] grouping_attributes (k8s.namespace.name, puis service.namespace, par défaut), donc sur Kubernetes le namespace où tournent les services. Filtre la ligne Analyse et rétrécit la liste Service. La valeur seule sert de label : deux attributs de même valeur partagent une entrée. grouping=\"_other\" est le bac de repli pour les paires (service, grouping) au-delà des plafonds de paires par exécution (512 sur les findings et les compteurs I/O d'analyse, 256 sur l'histogramme des spans lents, 4096 sur le compteur d'ingestion) ; une paire repliée garde son service. Un span qui ne portait aucun des attributs configurés a une valeur vide, que Prometheus stocke comme une absence de label : ces séries ne sont visibles qu'avec All.")

t("description",
  "Analysed service (OTLP service.name). Filters the Analysis row, the Daemon health row is daemon-wide. service=\"_other\" is the fold bucket for services past the per-run cardinality caps (128 on findings, 64 on the slow-span histogram): a service admitted at ingest but past an analysis cap has its findings there, not under its own name. Narrowed by Grouping.",
  "Service analysé (service.name OTLP). Filtre la ligne Analyse ; la ligne Santé du daemon est à l'échelle du daemon. service=\"_other\" est le bac de repli pour les services au-delà des plafonds de cardinalité par exécution (128 sur les findings, 64 sur l'histogramme des spans lents) : un service admis à l'ingestion mais au-delà d'un plafond d'analyse a ses findings là, pas sous son propre nom. Rétréci par Namespace des services.")


# --- 0.20.0: service silence ---
t("title", "Service silence (time since last span)",
  "Silence des services (temps depuis le dernier span)")

t("description",
  "Seconds since the daemon last received a span for each service, from `perf_sentinel_service_last_span_timestamp_seconds`.\n\nA traffic signal, not a liveness one: a crash, a scale to zero, a rolling deploy and a quiet cron all look the same, so alert on it with a `for:` longer than the service's normal idle gap.\n\nRising beside a flat findings rate is the shape of a service that stopped, and the window to hand to `/api/findings?since_ms=&until_ms=`.",
  "Secondes écoulées depuis le dernier span reçu par le daemon pour chaque service, d'après `perf_sentinel_service_last_span_timestamp_seconds`.\n\nUn signal de trafic et non de vivacité : un plantage, une mise à zéro des réplicas, un déploiement progressif et une tâche planifiée silencieuse se ressemblent tous, donc alerter dessus avec un `for:` plus long que le creux normal du service.\n\nUne montée à côté d'un débit de findings plat est la forme d'un service qui s'est arrêté, et la fenêtre à passer à `/api/findings?since_ms=&until_ms=`.")


# --- findings dashboard (query API, through the Infinity plugin) ---
f("title", "Daemon status", "État du daemon")
f("title", "Daemon acknowledgments", "Acquittements du daemon")
f("title", "Energy backends", "Backends d'énergie")
f("label", "Max rows", "Lignes max")
f("label", "Include acked", "Inclure les acquittés")

f("description",
  "Findings from the perf-sentinel query API through the Infinity datasource in examples/grafana-infinity-datasource.yaml. 'perf-sentinel overview' counts findings by kind from Prometheus, this one names the operation and endpoint.\n\nNo embedded IAM on the API: scope the folder to people allowed to see your SQL templates and endpoint names.\n\nIncidents and Incident findings ignore the time picker and need [daemon] read_api_key, sent as X-API-Key. Correlations needs [daemon.correlation] enabled and ignores the variables.\n\nGrouping and Service are the overview dashboard's labels, sent as exact matches. Skip rows pages past the 1000-row cap.",
  "Findings lus depuis l'API de requêtage de perf-sentinel via le datasource Infinity de examples/grafana-infinity-datasource.yaml. 'Perf Sentinel' compte les findings par type depuis Prometheus, ce dashboard nomme l'opération et l'endpoint.\n\nL'API n'a aucune gestion d'identité intégrée : restreindre le dossier du dashboard aux personnes autorisées à voir les modèles de requêtes SQL et les noms d'endpoints.\n\nIncidents et Findings de l'incident ignorent le sélecteur de temps et demandent [daemon] read_api_key, envoyé en X-API-Key. Corrélations demande que [daemon.correlation] soit activé et ignore les variables.\n\nNamespace des services et Service sont les labels du dashboard d'aperçu, envoyés en correspondance exacte. Lignes à sauter pagine au-delà du plafond de 1000 lignes.")

f("description",
  "The Infinity datasource pointing at the perf-sentinel daemon's query API, provisioned by examples/grafana-infinity-datasource.yaml.",
  "Le datasource Infinity qui pointe sur l'API de requêtage du daemon perf-sentinel, provisionné par examples/grafana-infinity-datasource.yaml.")

f("description",
  "Server-side cap, applied after the API folds findings by signature. The daemon refuses anything above 1000. Pair with Skip rows to read past it.",
  "Plafond appliqué côté serveur, après le regroupement des findings par signature. Le daemon refuse toute valeur au-dessus de 1000. À combiner avec Lignes à sauter pour lire au-delà.")

f("description",
  "Since 0.5.20 the API leaves acknowledged findings out by default, so a critical problem someone acked is absent from the table with nothing saying so. true asks for them back, and the 'Acked via' column names the source (toml for the CI baseline, daemon for the runtime store), the same union the acknowledgments table below lists.",
  "Depuis la 0.5.20, l'API laisse par défaut les findings acquittés de côté : un problème critique que quelqu'un a acquitté est donc absent du tableau sans que rien ne le dise. true les redemande, et la colonne Acquitté via nomme l'origine (toml pour la base de référence de la CI, daemon pour le stock d'exécution), la même union que liste le tableau des acquittements plus bas.")

f("description",
  "GET /api/status, the daemon's own liveness object: binary version, seconds since start, and three runtime gauges each followed by its cap (correlation window, analysis queue, findings ring buffer).\n\nAn uptime reset is a restart, which also empties the acknowledgments table below unless [daemon.ack] persists to disk.",
  "GET /api/status, l'objet de vivacité que le daemon publie sur lui-même : version du binaire, secondes depuis le démarrage, et trois jauges d'exécution chacune suivie de son plafond (fenêtre de corrélation, file d'analyse, tampon circulaire des findings).\n\nUn uptime qui repart de zéro est un redémarrage, qui vide aussi le tableau des acquittements plus bas si [daemon.ack] ne persiste pas sur disque.")

f("description",
  "One row per distinct problem, folded by the signature acknowledgments use. Filter with the column headers.\n\n'Traces' counts retained detections: it falls as they age out and resets on restart, so it measures presence, not a lifetime total. 'Occurrences' is the repeats inside the trace shown. 'Est. impact (ops)' is Traces x Ops/trace, the avoidable I/O to rank by, 0 for the types that waste none.\n\n'Suggestion' is the fix for the framework or broker inferred from the trace when there is one, the generic hint otherwise, and 'Fix for' names that technology, blank when the hint is generic. 'Code' is the call site from the span's code.* attributes, the function then file:line as the CLI prints them, either one alone when the other is missing, blank when the instrumentation sends none.\n\n'Acked via' needs Include acked true: toml is the CI baseline file, daemon the runtime store. Grouping, Service and Finding type narrow the request, so a tenant is complete up to Max rows, and Skip rows reads on.",
  "Une ligne par problème distinct, regroupée sur la signature qu'utilisent les acquittements. Filtrer avec les en-têtes de colonnes.\n\n'Traces' compte les détections retenues : il baisse à mesure qu'elles expirent et repart de zéro au redémarrage, donc il mesure la présence et non un total depuis toujours. 'Occurrences' est le nombre de répétitions à l'intérieur de la trace affichée. 'Impact est. (ops)' vaut Traces x Ops/trace, soit les I/O évitables sur lesquelles trier, et 0 pour les types qui n'en gaspillent aucune.\n\n'Suggestion' est le correctif pour le framework ou le broker déduit de la trace quand il y en a un, sinon le conseil générique, et 'Correctif pour' nomme cette technologie, vide quand le conseil est générique. 'Code' est l'emplacement de l'appel lu dans les attributs code.* du span : la fonction puis fichier:ligne, comme les affiche la CLI, l'un des deux seul quand l'autre manque, vide quand l'instrumentation n'envoie rien.\n\n'Acquitté via' ne se remplit que si Inclure les acquittés vaut true : toml est le fichier de référence de la CI, daemon le stock d'exécution. Namespace des services, Service et Type de finding restreignent la requête, donc un tenant est complet jusqu'à Lignes max, et Lignes à sauter lit la suite.")

f("label", "Skip rows", "Lignes à sauter")
f("label", "Finding type", "Type de finding")

f("description",
  "Finding type, sent to the API as an exact match on type, under the labels the Type column shows. All sends a single space, which the API reads as no filter, like Grouping and Service. Narrowing on the daemon keeps a rare type such as Slow SQL from being cut by Max rows, where the Type column header only filters the page already returned. The twelve types are fixed in the dashboard, so a type added by a later daemon is missing here but still listed under All.",
  "Type de finding, envoyé à l'API en correspondance exacte sur type, sous les libellés de la colonne Type. All envoie une simple espace, que l'API lit comme l'absence de filtre, comme Namespace des services et Service. Restreindre côté daemon évite qu'un type rare comme SQL lent soit coupé par Lignes max, là où l'en-tête de la colonne Type ne filtre que la page déjà renvoyée. Les douze types sont figés dans le dashboard : un type ajouté par un daemon plus récent manque ici mais reste listé sous All.")

# Options of the Finding type picker, in Grafana's "label : value" form.
f("query",
  "N+1 SQL : n_plus_one_sql,N+1 HTTP : n_plus_one_http,N+1 messaging : n_plus_one_messaging,Redundant SQL : redundant_sql,Redundant HTTP : redundant_http,Slow SQL : slow_sql,Slow HTTP : slow_http,Slow messaging : slow_messaging,Excessive fanout : excessive_fanout,Chatty service : chatty_service,Pool saturation : pool_saturation,Serialized calls : serialized_calls",
  "N+1 SQL : n_plus_one_sql,N+1 HTTP : n_plus_one_http,N+1 messaging : n_plus_one_messaging,SQL redondant : redundant_sql,HTTP redondant : redundant_http,SQL lent : slow_sql,HTTP lent : slow_http,Messaging lent : slow_messaging,Fanout excessif : excessive_fanout,Service bavard : chatty_service,Saturation de pool : pool_saturation,Appels sérialisés : serialized_calls")

f("description",
  "The Prometheus instance scraping the daemon's /metrics. It feeds the Grouping and Service variables alone, from the labels of perf_sentinel_findings_total, the same ones the overview dashboard filters on; every table reads the daemon through DS_INFINITY.",
  "L'instance Prometheus qui scrape le /metrics du daemon. Elle n'alimente que les variables Namespace des services et Service, depuis les labels de perf_sentinel_findings_total, les mêmes que filtre le dashboard d'aperçu ; tous les tableaux lisent le daemon via DS_INFINITY.")

f("description",
  "Effective grouping of the analysed traffic: the first attribute present from [detection] grouping_attributes (k8s.namespace.name, then service.namespace, by default), so on Kubernetes the namespace the workloads run in. Listed from the grouping label of perf_sentinel_findings_total, so it offers exactly what the overview dashboard filters on. Sent to the API as an exact match. All sends a single space, which the API reads as no filter: Grafana ignores an empty allValue, so a blank is the only value that reaches the request. The Prometheus datasource serves this list and the Service list, nothing else. grouping=\"_other\" is the metrics' fold bucket for pairs past the per-run caps, not a value a finding carries: picking it yields an empty table, the rows it stands for sit under their real grouping.",
  "Regroupement effectif des charges analysées : le premier attribut présent parmi [detection] grouping_attributes (k8s.namespace.name, puis service.namespace, par défaut), donc sur Kubernetes le namespace où tournent les charges. Listé depuis le label grouping de perf_sentinel_findings_total, il propose donc exactement ce que filtre le dashboard d'aperçu. Envoyé à l'API en correspondance exacte. All envoie une simple espace, que l'API lit comme l'absence de filtre : Grafana ignore une allValue vide, un blanc est donc la seule valeur qui atteigne la requête. Le datasource Prometheus ne sert que cette liste et celle de Service. grouping=\"_other\" est le bac de repli des métriques pour les paires au-delà des plafonds par exécution, pas une valeur que porte un finding : le choisir donne un tableau vide, les lignes qu'il représente étant sous leur vrai regroupement.")

f("description",
  "Analysed service (OTLP service.name), listed from the service label of perf_sentinel_findings_total. Sent to the API as an exact match. All sends a single space, which the API reads as no filter: Grafana ignores an empty allValue, so a blank is the only value that reaches the request. Not narrowed by Grouping: a service absent from the chosen grouping yields an empty table.",
  "Service analysé (service.name OTLP), listé depuis le label service de perf_sentinel_findings_total. Envoyé à l'API en correspondance exacte. All envoie une simple espace, que l'API lit comme l'absence de filtre : Grafana ignore une allValue vide, un blanc est donc la seule valeur qui atteigne la requête. Non rétréci par Namespace des services : un service absent du regroupement choisi donne un tableau vide.")

f("description",
  "The API's offset: folded rows skipped before the page starts, so a listing past the 1000-row cap is read in slices. Add Max rows each time: 0, then 200, 400 at the default, or 0, 1000, 2000 at the cap. The ring keeps moving between two requests, so a row can cross a slice boundary; narrowing with Grouping and Service first is what makes most pages fit under the cap.",
  "L'offset de l'API : lignes regroupées sautées avant le début de la page, pour lire en tranches une liste qui dépasse le plafond de 1000 lignes. Ajouter Lignes max à chaque fois : 0, puis 200, 400 au défaut, ou 0, 1000, 2000 au plafond. Le tampon circulaire bouge entre deux requêtes, donc une ligne peut passer d'une tranche à l'autre ; restreindre d'abord avec Namespace des services et Service est ce qui fait tenir la plupart des pages sous le plafond.")

f("description",
  "Findings acknowledged at runtime, from GET /api/acks, usually why a problem is absent from the table above. Empty when [daemon.ack] is disabled. If the ack API is keyed, [daemon] read_api_key opens this route without the power to ack.",
  "Findings acquittés à l'exécution, depuis GET /api/acks, en général la raison pour laquelle un problème est absent du tableau au-dessus. Vide quand [daemon.ack] est désactivé. Si l'API d'acquittement est protégée par une clé, [daemon] read_api_key ouvre cette route sans donner le droit d'acquitter.")

f("description",
  "GET /api/energy, one row per energy or intensity backend in precedence order: configured (from [green], frozen at startup), seconds since the last successful scrape, and scrape successes and failures since daemon start.\n\nBlank age and counts mean not applicable (unconfigured or never scraped, as cloud_energy and electricity_maps), 0 means fresh, and age 0 with 0 successes is a first scrape interval still pending. With every row unconfigured, carbon figures come from the built-in I/O proxy.",
  "GET /api/energy, une ligne par backend d'énergie ou d'intensité carbone, dans l'ordre de priorité : s'il est configuré (depuis [green], figé au démarrage), les secondes écoulées depuis le dernier relevé réussi, et les succès et échecs de relevé depuis le démarrage du daemon.\n\nUn âge et des compteurs vides veulent dire sans objet (non configuré ou jamais relevé, comme cloud_energy et electricity_maps), un 0 veut dire frais, et un âge de 0 avec 0 succès est un premier intervalle de relevé encore en attente. Si aucune ligne n'est configurée, les chiffres carbone viennent du proxy I/O interne.")

# Column headers of the findings dashboard. Translated by replacing the whole
# JSON string rather than the `text` field alone: the same names key the
# organize and calculateField transformations, and translating only the header
# would break the column order and the computed column with no error anywhere.
# check_columns() below verifies that no reference dangles.
# --- 0.20.0: incidents frozen by alerting ---
f("description",
  "Id of the incident whose findings the Incident findings table shows. Set by clicking a row of the Incidents table, or paste one from GET /api/incidents. Empty leaves that table without data.",
  "Id de l'incident dont le tableau Findings de l'incident montre les findings. Renseigné en cliquant une ligne du tableau Incidents, ou collé depuis GET /api/incidents. Vide, ce tableau reste sans données.")

f("description",
  "GET /api/incidents, newest first, one row per incident your alerting posted, opt-in via [daemon.incidents]. A page of 50, starting after 'Incident skip rows' incidents, so 0 shows the 50 most recent. The time picker does not apply.\n\nAsked with findings=false, so each incident arrives with finding_count in place of its frozen findings, which keeps the response small however many findings the page froze. A daemon that ignores the parameter still sends the findings, and the panel counts them instead.\n\n'Findings' is the number of rows frozen between Window from and Window to, closed two trace TTLs after the incident. 'Capture' reads Ring reached against Window from: partial means the ring had evicted part of the window (the NDJSON archive may still answer), empty ring means nothing retained. 'Ended' is blank while the alert fires.\n\nClick a row to load its findings below (Incident variable). Sent with [daemon] read_api_key as X-API-Key, never the key your alerting posts with.",
  "GET /api/incidents, du plus récent au plus ancien, une ligne par incident posté par votre alerting, à activer par [daemon.incidents]. Une page de 50, qui commence après 'Incidents à sauter' incidents, donc 0 montre les 50 plus récents. Le sélecteur de temps ne s'applique pas.\n\nDemandé avec findings=false : chaque incident arrive avec finding_count à la place de ses findings figés, ce qui garde la réponse petite quel que soit le nombre de findings que la page a figés. Un daemon qui ignore le paramètre envoie quand même les findings, que le panneau compte alors.\n\n'Findings' est le nombre de lignes figées entre Fenêtre du et Fenêtre au, une fenêtre qui se ferme deux TTL de trace après l'incident. 'Capture' compare Tampon remonte à avec Fenêtre du : partial veut dire que le tampon avait évincé une partie de la fenêtre (l'archive NDJSON peut encore répondre), empty ring veut dire que rien n'a été retenu. 'Fin' reste vide tant que l'alerte est active.\n\nCliquer une ligne charge ses findings plus bas (variable Incident). Le datasource envoie [daemon] read_api_key en X-API-Key, jamais la clé avec laquelle votre alerting poste.")

f("title", "Show this incident's findings", "Voir les findings de cet incident")

f("title", "Incident findings", "Findings de l'incident")


# --- new panels and labels, Prometheus dashboard ---
t("title", "Compatibility", "Compatibilité")

t("description",
  "Whether the daemon feeding this dashboard is recent enough for its panels. It reads the presence of two metrics the panels depend on rather than a version number, because no perf-sentinel metric carries one.\n\n`perf_sentinel_incidents_total` arrived in 0.20.0 and `perf_sentinel_analysis_service_overflow_total` in 0.18.0. Both are published from daemon start whatever the configuration, so their absence dates the binary. Dropping either one in a scrape relabel rule reads here as an old daemon.\n\nPartly compatible means 'Service silence' stays empty. Not compatible means the Service and Grouping pickers offer names the findings panels cannot honour, so a filtered view reads as a clean service.\n\nEmpty panels on a current daemon are configuration, not age: `[green] enabled = false` silences the waste ratio and the avoidable counters, and `[daemon] per_service_labels = false` or `per_grouping_labels = false` empty the two pickers.",
  "Indique si le daemon qui alimente ce dashboard est assez récent pour ses panneaux. La vérification repose sur la présence de deux métriques dont dépendent les panneaux, et non sur un numéro de version, parce qu'aucune métrique perf-sentinel n'en porte.\n\nLa métrique `perf_sentinel_incidents_total` est apparue en 0.20.0 et `perf_sentinel_analysis_service_overflow_total` en 0.18.0. Les deux sont publiées dès le démarrage du daemon, quelle que soit la configuration, donc leur absence indique l'âge du binaire. Si une règle de relabel retire l'une des deux au relevé, le daemon apparaît ici comme ancien.\n\nPartiellement compatible veut dire que 'Silence des services' reste vide. Non compatible veut dire que les sélecteurs Service et Namespace des services proposent des noms que les panneaux de findings ne peuvent pas prendre en compte, si bien qu'une vue filtrée donne l'impression d'un service sans problème.\n\nDes panneaux vides sur un daemon à jour viennent de la configuration, pas de l'âge : `[green] enabled = false` rend muets le ratio de gaspillage et les compteurs d'opérations évitables, et `[daemon] per_service_labels = false` ou `per_grouping_labels = false` vident les deux sélecteurs.")

t("legendFormat", "Dashboard compatibility", "Compatibilité du dashboard")

t("noValue", "Unknown", "Inconnu")

# --- display names of finding types and states, Prometheus dashboard ---
t("text", "Compatible", "Compatible")
t("text", "Partly compatible", "Partiellement compatible")
t("text", "Not compatible", "Non compatible")
t("text", "N+1 SQL", "N+1 SQL")
t("text", "N+1 HTTP", "N+1 HTTP")
t("text", "N+1 messaging", "N+1 messaging")
t("text", "Redundant SQL", "SQL redondant")
t("text", "Redundant HTTP", "HTTP redondant")
t("text", "Slow SQL", "SQL lent")
t("text", "Slow HTTP", "HTTP lent")
t("text", "Slow messaging", "Messaging lent")
t("text", "Excessive fanout", "Fanout excessif")
t("text", "Chatty service", "Service bavard")
t("text", "Pool saturation", "Saturation de pool")
t("text", "Serialized calls", "Appels sérialisés")

# --- new panels and labels, findings dashboard ---
f("title", "Compatibility", "Compatibilité")

f("description",
  "Whether the daemon answering the query API is recent enough for this dashboard. It reads `version` from `GET /api/status`, which every daemon since 0.4.0 returns.\n\nBelow 0.21.0 this dashboard misreads rather than degrades. `grouping` and `offset` did not exist as query parameters, and the API ignores what it does not know, so the Grouping pill names one tenant while the table lists every one of them and 'Skip rows' returns the same page at every value. The 'All' option sends a single space, which such a daemon takes as an exact match and answers with nothing.\n\nEmpty columns on a current daemon are configuration, not age: `[daemon.correlation]` off empties Correlations, no `[daemon.incidents]` answers 503 on both incident panels, and green scoring off empties 'Ops/trace'.",
  "Indique si le daemon derrière l'API de requêtage est assez récent pour ce dashboard. Le panneau lit `version` dans `GET /api/status`, que tous les daemons renvoient depuis la 0.4.0.\n\nAvant la 0.21.0, ce dashboard se trompe au lieu de se dégrader. `grouping` et `offset` n'existaient pas comme paramètres de requête, et l'API ignore ce qu'elle ne connaît pas : le sélecteur Namespace des services désigne donc un seul tenant alors que le tableau les liste tous, et 'Lignes à sauter' renvoie la même page quelle que soit la valeur. L'option 'All' envoie une simple espace, qu'un tel daemon prend pour une correspondance exacte, et il ne renvoie alors rien.\n\nSur un daemon à jour, des colonnes vides viennent de la configuration et non de l'ancienneté : `[daemon.correlation]` coupé laisse Corrélations vide, sans `[daemon.incidents]` l'API répond 503 sur les deux panneaux d'incidents, et le scoring green coupé laisse 'Ops/trace' vide.")

f("noValue", "Unknown", "Inconnue")

f("title", "Correlations", "Corrélations")

f("description",
  "GET /api/correlations: source-target pairs of findings fired together across services in the same grouping, the target within lag_threshold_ms of the source. Empty until [daemon.correlation] enabled is true. Pairs are not persisted: gone window_minutes after the last co-occurrence and on restart.\n\nConfidence is Co-occurrences over Source total, 100% when the target always followed. Median lag is the typical delay. Operation columns are SQL or HTTP templates, not routes. Source trace and Target trace are the two sides of the latest co-occurrence, for /api/explain.\n\nGrouping and Service do not narrow this table, the headers do. Capped at 1000 rows, highest confidence first.",
  "GET /api/correlations : paires source-cible de findings déclenchés ensemble entre services d'un même regroupement, la cible à moins de lag_threshold_ms de la source. Vide tant que [daemon.correlation] enabled ne vaut pas true. Les paires ne sont pas persistées : elles disparaissent window_minutes après la dernière co-occurrence, et au redémarrage.\n\nConfiance vaut Co-occurrences divisé par Total source, 100 % quand la cible a toujours suivi. Délai médian donne le délai habituel. Les colonnes d'opération sont des modèles SQL ou HTTP, pas des routes. Trace source et Trace cible sont les deux côtés de la dernière co-occurrence, pour /api/explain.\n\nNamespace des services et Service ne restreignent pas ce tableau, ce sont les en-têtes qui le font. Plafonné à 1000 lignes, par confiance décroissante.")

# --- display names of finding types and states, findings dashboard ---
f("text", "Daemon $1 too old", "Daemon $1 trop ancien")
f("text", "Compatible, $1", "Compatible, $1")
f("text", "N+1 SQL", "N+1 SQL")
f("text", "N+1 HTTP", "N+1 HTTP")
f("text", "N+1 messaging", "N+1 messaging")
f("text", "Redundant SQL", "SQL redondant")
f("text", "Redundant HTTP", "HTTP redondant")
f("text", "Slow SQL", "SQL lent")
f("text", "Slow HTTP", "HTTP lent")
f("text", "Slow messaging", "Messaging lent")
f("text", "Excessive fanout", "Fanout excessif")
f("text", "Chatty service", "Service bavard")
f("text", "Pool saturation", "Saturation de pool")
f("text", "Serialized calls", "Appels sérialisés")


t("description",
  "Operational emissions for the same window, summed across regions: energy multiplied by the local grid intensity.\n\nIt moves with the grid as well as the workload, so a drop can be a cleaner hour rather than an optimisation. Manufacturing emissions are excluded.",
  "Émissions opérationnelles pour la même fenêtre, sommées sur les régions : l'énergie multipliée par l'intensité carbone du réseau local.\n\nElles bougent avec la charge mais aussi avec le réseau électrique, donc une baisse peut être une heure plus propre plutôt qu'une optimisation. Les émissions de fabrication sont exclues.")

f("description",
  "The findings frozen for the incident selected above, from GET /api/incidents?id=, whatever page the Incidents table shows. A daemon before 0.24.0 ignores id and answers the Incidents table's page, where a clicked row is found too. Empty until a row is clicked or Incident holds an id. The time picker does not apply.\n\nOne row per signature folded over the incident's window alone, so Traces and First seen describe the window, not the whole ring. The settle pass can add rows and raise counts, never remove. A First seen after Started fired only after the restart.\n\nSent with [daemon] read_api_key as X-API-Key. Est. impact, Suggestion, Fix for, Code and the column order follow the Findings table above.",
  "Les findings figés pour l'incident sélectionné ci-dessus, lus depuis GET /api/incidents?id=, quelle que soit la page affichée par le tableau Incidents. Un daemon antérieur à la 0.24.0 ignore id et renvoie la page du tableau Incidents, où une ligne cliquée se trouve aussi. Vide jusqu'à ce qu'une ligne soit cliquée ou qu'Incident porte un id. Le sélecteur de temps ne s'applique pas.\n\nUne ligne par signature, repliée sur la seule fenêtre de l'incident, donc Traces et Première détection décrivent la fenêtre et non tout le tampon. La passe de stabilisation peut ajouter des lignes et augmenter des compteurs, jamais en retirer. Une ligne dont la Première détection est postérieure au Début ne s'est déclenchée qu'après le redémarrage.\n\nRequête envoyée avec [daemon] read_api_key en X-API-Key. Impact est., Suggestion, Correctif pour, Code et l'ordre des colonnes suivent le tableau Findings ci-dessus.")

# --- 0.24.0: History (Hub) row, ack links, incident pages ---
f("label", "Incident skip rows", "Incidents à sauter")

f("description",
  "The API's offset for the Incidents table (Incident findings asks by id, and reads this page only from a daemon before 0.24.0): incidents skipped, newest first, before the page of 50 starts. 0, then 50, 100 to page back through a ring of up to 1000 ([daemon.incidents] max_retained). A new incident shifts every page by one row.",
  "Le décalage de l'API pour le tableau Incidents (Findings de l'incident demande par id, et ne lit cette page qu'auprès d'un daemon antérieur à la 0.24.0) : incidents sautés, du plus récent au plus ancien, avant le début de la page de 50. 0, puis 50, 100 pour remonter un tampon d'au plus 1000 incidents ([daemon.incidents] max_retained). Un nouvel incident décale chaque page d'une ligne.")

f("label", "perf-sentinel Hub", "Hub perf-sentinel")

f("description",
  "Optional. The Infinity datasource that points at PerfSentinelHub, read by the History (Hub) row alone. Only datasources whose name contains Hub are offered, so the row never binds to the daemon by accident. Left empty, the row errors when opened and nothing else changes.",
  "Facultatif. Le datasource Infinity qui pointe vers PerfSentinelHub, lu par la seule ligne Historique (Hub). Seuls les datasources dont le nom contient Hub sont proposés, pour que la ligne ne se branche jamais sur le daemon par erreur. Laissé vide, la ligne est en erreur quand on l'ouvre et rien d'autre ne change.")

f("label", "Environment", "Environnement")

f("description",
  "Environment of the History (Hub) row, one of the environments the Hub's sources declare, listed from the environment label of perf_sentinel_hub_findings, so Prometheus has to scrape the Hub. All sends a single space, which the Hub reads as the whole fleet. The Hub answers 400 to an environment it does not know.",
  "Environnement de la ligne Historique (Hub), parmi ceux que déclarent les sources du Hub, listés depuis le label environment de perf_sentinel_hub_findings : Prometheus doit donc scraper le Hub. All envoie une simple espace, que le Hub lit comme tout le parc. Le Hub répond 400 à un environnement qu'il ne connaît pas.")

f("label", "Hub skip rows", "Lignes Hub à sauter")

f("description",
  "The Hub's offset for the History (Hub) row: findings skipped, most recently seen first, before the page of Max rows starts.",
  "Le décalage du Hub pour la ligne Historique (Hub) : findings sautés, du plus récemment vu au plus ancien, avant le début de la page de Lignes max.")

f("label", "Hub URL", "URL du Hub")

f("description",
  "Origin of the Hub launcher as your browser reaches it, for example https://hub.example.internal, without a trailing slash. The Ack links of the three findings tables use it to open the Hub's ack page. Empty leaves those links dead.",
  "Origine du lanceur du Hub telle que votre navigateur l'atteint, par exemple https://hub.example.internal, sans barre oblique finale. Les liens Acquitter des trois tableaux de findings l'utilisent pour ouvrir la page d'acquittement du Hub. Vide, ces liens ne mènent nulle part.")

f("title", "Ack or revoke in the Hub", "Acquitter ou révoquer dans le Hub")
f("title", "History (Hub)", "Historique (Hub)")
f("title", "Findings history", "Historique des findings")

f("description",
  "One row per problem the Hub saw in the chosen environment inside the dashboard's time range, read from the Hub rather than from the daemon's ring. A day counts when the Hub observed the finding that day, on the Hub's clock in UTC.\n\n'First seen', 'Last seen' and 'Status' describe the chosen environment alone, the whole fleet on All. There are no occurrence columns: the daemon's counts are relative to its ring and do not add up over months. 'Code' is the call site as the Findings table above shows it, at the last observation.\n\n'Acked on' and 'Acked by' come from the Hub's mirror of each daemon's acks, 'Acked via' is the daemon's own annotation at the last observation. The Ack link opens the Hub's ack page for that signature, once Hub URL is set.\n\nNeeds PerfSentinelHub 0.3.0: an older Hub ignores the range and the environment and answers its latest state.",
  "Une ligne par problème que le Hub a vu dans l'environnement choisi pendant la période du dashboard, lue depuis le Hub et non depuis le tampon du daemon. Un jour compte quand le Hub a observé le finding ce jour-là, selon l'horloge du Hub en UTC.\n\n'Première détection', 'Vu le' et 'Statut' décrivent le seul environnement choisi, tout le parc sur All. Il n'y a pas de colonne d'occurrences : les compteurs du daemon sont relatifs à son tampon et ne s'additionnent pas sur des mois. 'Code' est l'emplacement de l'appel tel que le montre le tableau Findings ci-dessus, à la dernière observation.\n\n'Acquitté le' et 'Acquitté par' viennent du miroir que le Hub tient des acquittements de chaque daemon, 'Acquitté via' est l'annotation du daemon lui-même à la dernière observation. Le lien Acquitter ouvre la page d'acquittement du Hub pour cette signature, une fois URL du Hub renseignée.\n\nDemande PerfSentinelHub 0.3.0 : un Hub plus ancien ignore la période et l'environnement et renvoie son dernier état.")

# Statuses of the History (Hub) row.
f("text", "Active", "Actif")
f("text", "Likely resolved", "Probablement résolu")
f("text", "Not observed", "Non observé")

# --- finding type legends, renamed by renameByRegex ---
t("renamePattern", "N+1 SQL$1", "N+1 SQL$1")
t("renamePattern", "N+1 HTTP$1", "N+1 HTTP$1")
t("renamePattern", "N+1 messaging$1", "N+1 messaging$1")
t("renamePattern", "Redundant SQL$1", "SQL redondant$1")
t("renamePattern", "Redundant HTTP$1", "HTTP redondant$1")
t("renamePattern", "Slow SQL$1", "SQL lent$1")
t("renamePattern", "Slow HTTP$1", "HTTP lent$1")
t("renamePattern", "Slow messaging$1", "Messaging lent$1")
t("renamePattern", "Excessive fanout$1", "Fanout excessif$1")
t("renamePattern", "Chatty service$1", "Service bavard$1")
t("renamePattern", "Pool saturation$1", "Saturation de pool$1")
t("renamePattern", "Serialized calls$1", "Appels sérialisés$1")


COLUMNS = [
    ("Severity", "Sévérité"),
    ("Operation", "Opération"),
    ("Acked via", "Acquitté via"),
    ("Last seen", "Vu le"),
    ("Est. impact (ops)", "Impact est. (ops)"),
    ("Active traces", "Traces actives"),
    ("Active traces cap", "Plafond traces actives"),
    ("Analysis queue", "File d'analyse"),
    ("Analysis queue cap", "Plafond file d'analyse"),
    ("Stored findings", "Findings stockés"),
    ("Stored findings cap", "Plafond findings stockés"),
    ("By", "Par"),
    ("Reason", "Motif"),
    ("Acknowledged", "Acquitté le"),
    ("Expires", "Expire le"),
    ("Configured", "Configuré"),
    ("Last scrape age (s)", "Âge du dernier relevé (s)"),
    ("Scrapes OK", "Relevés OK"),
    ("Scrapes failed", "Relevés en échec"),
    ("Started", "Début"),
    ("Ended", "Fin"),
    ("Kind", "Nature"),
    ("Window from", "Fenêtre du"),
    ("Window to", "Fenêtre au"),
    ("Ring reached", "Tampon remonte à"),
    ("Detail", "Détail"),
    ("First seen", "Première détection"),
    ("Grouping", "Namespace des services"),
    ("Compatibility", "Compatibilité"),
    ("Fix for", "Correctif pour"),
    ("Ack", "Acquitter"),
    ("Status", "Statut"),
    ("Acked on", "Acquitté le"),
    ("Acked by", "Acquitté par"),
    ("Source trace", "Trace source"),
    ("Target trace", "Trace cible"),
    ("Source service", "Service source"),
    ("Source type", "Type source"),
    ("Target service", "Service cible"),
    ("Target type", "Type cible"),
    ("Confidence", "Confiance"),
    ("Co-occurrences", "Co-occurrences"),
    ("Source total", "Total source"),
    ("Median lag", "Délai médian"),
    ("Source operation", "Opération source"),
    ("Target operation", "Opération cible"),
]

# Stays English on purpose: metric, label and API field names, snake_case
# legends, variable values, and the column headers spelled the same in both
# languages.
KEEP_EN = {
    "Prometheus", "Job", "Service", "perf-sentinel overview",
    "perf-sentinel findings", "perf-sentinel API", "Findings", "total", "Type", "Endpoint",
    "Traces", "Occurrences", "Ops/trace", "Trace", "Suggestion", "Signature", "Backend",
    "Version", "Uptime", "SQL p95", "HTTP p95", "Messaging p95",
    "Incident", "Incidents", "Namespace", "Capture", "Id", "Sources", "Code",
    # Variable options and states already left in English.
    "All", "true", "false", "DOWN", "UP", "OK", "REJECTING",
}


def translate_columns(text):
    """Renames the findings dashboard's columns everywhere they appear."""
    renamed = 0
    for en, fr in COLUMNS:
        en_key, fr_key = json.dumps(en, ensure_ascii=False), json.dumps(fr, ensure_ascii=False)
        if en_key in text and en != fr:
            text = text.replace(en_key, fr_key)
            renamed += 1
    return text, renamed


def check_columns(dashboard):
    """Every column a transformation names must exist on its panel, otherwise
    the column order or the computed column dies silently."""
    broken = []
    for panel in dashboard.get("panels", []):
        names = {c.get("text") for tgt in panel.get("targets", []) for c in tgt.get("columns", [])}
        for tr in panel.get("transformations", []):
            opts = tr.get("options", {})
            if tr.get("id") == "calculateField":
                binary = opts.get("binary", {})
                for side in ("left", "right"):
                    ref = binary.get(side)
                    if isinstance(ref, str) and ref and ref not in names:
                        broken.append("%s: calculateField %s -> %s" % (panel.get("title"), side, ref))
                names.add(opts.get("alias"))
            if tr.get("id") == "organize":
                for ref in list(opts.get("indexByName", {})) + list(opts.get("excludeByName", {})):
                    if ref not in names:
                        broken.append("%s: organize -> %s" % (panel.get("title"), ref))
    return broken


def check_coverage(dashboard, table):
    """Visible strings no entry covers: a panel added to the English file
    would stay English with nothing saying so."""
    covered = {v for _, en, fr in table for v in (en, fr)}
    covered |= {v for en, fr in COLUMNS for v in (en, fr)}
    # text covers value mapping texts and column headers.
    fields = ("title", "description", "label", "noValue", "legendFormat", "tooltip",
              "text", "displayName", "renamePattern")
    missing = []

    def walk(node):
        if isinstance(node, dict):
            for k, v in node.items():
                if k in fields and isinstance(v, str) and v.strip():
                    technical = ("{{" in v or v.endswith("/s") or v.startswith("$")
                                 or not re.search(r"[A-Za-z]", v))
                    if v not in covered and v not in KEEP_EN and not technical:
                        missing.append("%s: %s" % (k, v[:70]))
                else:
                    walk(v)
        elif isinstance(node, list):
            for v in node:
                walk(v)

    walk(dashboard)
    return missing


# --- link menu between the two dashboards ---
t("title", "perf-sentinel dashboards", "Dashboards perf-sentinel")
f("title", "perf-sentinel dashboards", "Dashboards perf-sentinel")

t("tooltip",
  "The companion dashboard sharing the perf-sentinel tag: perf-sentinel findings reads the query API and answers which operation on which endpoint.",
  "Le dashboard voisin qui porte le tag perf-sentinel : Perf Sentinel findings lit l'API de requêtage et répond quelle opération sur quel endpoint.")

f("tooltip",
  "The companion dashboard sharing the perf-sentinel tag: perf-sentinel overview reads Prometheus and answers how many findings, of what kind, and how the daemon is doing.",
  "Le dashboard voisin qui porte le tag perf-sentinel : Perf Sentinel lit Prometheus et répond combien de findings, de quel type, et comment se porte le daemon.")


def key(field, value):
    return '"%s": %s' % (field, json.dumps(value, ensure_ascii=False))


def translate(text, table):
    """Returns (text, translated, unknown). unknown lists strings found in
    neither language, which is what an upstream rewording looks like."""
    translated, unknown = 0, []
    for field, en, fr in table:
        en_key, fr_key = key(field, en), key(field, fr)
        if en_key in text:
            # A label spelled the same in both languages is not a translation.
            if en != fr:
                text = text.replace(en_key, fr_key)
                translated += 1
        elif fr_key not in text:
            unknown.append("%s: %s" % (field, en[:70]))
    return text, translated, unknown


def check_overrides(dashboard):
    """Every byName override must name a legendFormat that still exists on its
    own panel, otherwise the override is dead and nothing says so."""
    broken = []
    for panel in dashboard.get("panels", []):
        legends = {t.get("legendFormat") for t in panel.get("targets", [])}
        # An Infinity table names columns where a graph names legends, and a
        # computed column only exists after its transformation.
        legends |= {c.get("text") for tgt in panel.get("targets", []) for c in tgt.get("columns", [])}
        legends |= {tr.get("options", {}).get("alias") for tr in panel.get("transformations", [])}
        # A Prometheus table names its columns after the labels of its by (...).
        for tgt in panel.get("targets", []):
            if tgt.get("format") == "table":
                for group in re.findall(r"\bby\s*\(([^)]*)\)", tgt.get("expr", "")):
                    legends |= {label.strip() for label in group.split(",")}
        for override in panel.get("fieldConfig", {}).get("overrides", []):
            matcher = override.get("matcher", {})
            if matcher.get("id") == "byName" and matcher.get("options") not in legends:
                broken.append("%s -> %s" % (panel.get("title"), matcher.get("options")))
    return broken


def main():
    check_only = "--check" in sys.argv[1:]
    failed = False

    for name, table, columns in (
        ("grafana-dashboard.json", TABLE, False),
        ("grafana-findings-dashboard.json", FINDINGS, True),
    ):
        source = os.path.join(EXAMPLES, name)
        target = os.path.join(EXAMPLES, "FR", name.replace(".json", "-FR.json"))
        text = io.open(source, encoding="utf-8").read()

        text, translated, unknown = translate(text, table)
        if columns:
            text, renamed = translate_columns(text)
            translated += renamed
        dashboard = json.loads(text)

        problems = ["string found in neither language: %s" % u for u in unknown]
        problems += ["byName override matches no legend: %s" % b for b in check_overrides(dashboard)]
        problems += ["transformation names no column: %s" % b for b in check_columns(dashboard)]
        problems += ["not covered by the table, would stay in English: %s" % m
                     for m in check_coverage(dashboard, table)]

        current = io.open(target, encoding="utf-8").read() if os.path.exists(target) else None
        if check_only and current != text:
            problems.append("%s is not what the English file and this table produce, rerun without --check"
                            % os.path.relpath(target, ROOT))

        print("%s: %d strings translated" % (os.path.relpath(target, ROOT), translated))
        for line in problems:
            print("  %s" % line)
        failed = failed or bool(problems)

        if not check_only and not problems and current != text:
            io.open(target, "w", encoding="utf-8", newline="\n").write(text)
            print("  written")

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
