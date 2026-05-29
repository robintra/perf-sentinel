# TUI interactif

`perf-sentinel` embarque un TUI interactif pour explorer les findings,
les arbres de spans et les corrélations cross-trace. Il expose trois
vues sous forme de drill-down : **Analyze** (le tableau de bord de
synthèse), **Inspect** (le navigateur multi-panneaux) et **Explain**
(l'arbre de spans plein écran d'une trace). Quel que soit le point
d'entrée, on circule entre les trois vues sans quitter le TUI.

Points d'entrée :

- `perf-sentinel analyze --tui [--input <events.json>]` : ouvre sur la
  vue Analyze.
- `perf-sentinel inspect --input <events.json>` : ouvre sur la vue
  Inspect, lit un fichier d'events brut ou un JSON Report pré-calculé.
- `perf-sentinel explain --tui --trace-id <id> --input <events.json>` :
  ouvre sur la vue Explain, centrée sur cette trace.
- `perf-sentinel query --daemon <URL> inspect` : mode live, ouvre sur
  Inspect, lit les findings et les traces depuis un daemon en cours
  d'exécution via HTTP.

En mode live (0.5.24+), le TUI permet aussi à l'opérateur d'acknowledger
et de révoquer des findings interactivement depuis le terminal.

![TUI all-in-one : Analyze descend vers Inspect puis Explain, Esc remonte](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/demo.gif)

## Vues et drill-down

Les trois vues forment un seul drill-down. `Enter` descend, `Esc` remonte :

```
Analyze  --Enter-->  Inspect  --Enter-->  Explain
         <---Esc---           <---Esc---
```

- **Analyze** : la synthèse scrollable (gaspillage GreenOps, principaux
  postes, quality gate), le même contenu que la sortie stdout d'`analyze`.
  `Enter` descend vers Inspect.
- **Inspect** : le navigateur multi-panneaux décrit ci-dessous. `Enter`
  parcourt les panneaux puis, depuis Detail, ouvre Explain. `Esc` remonte
  vers Analyze.
- **Explain** : l'arbre de spans annoté de la trace sélectionnée, plein
  écran et scrollable. `Esc` revient au panneau Detail d'Inspect.

Une barre d'onglets en haut met en évidence la vue active. Les arbres de
spans nécessitent des spans bruts (`inspect --input <events>.json` ou
`query inspect`). Un Report pré-calculé n'en porte pas, donc Explain
affiche un indice à la place.

![Vue Analyze : le tableau de bord de synthèse GreenOps sous la barre d'onglets](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/analyze.png)

![Vue Inspect : le navigateur à quatre panneaux, traces, findings, corrélations et detail](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/inspect.png)

![Vue Explain : l'arbre de spans annoté plein écran d'une trace](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/explain.png)

## Layout

L'écran se découpe en une layout 2 lignes :

```
┌─ Traces ──┬─ Findings ────────────────┬─ Correlations ────┐
│ trace-1   │ [1] N+1 SQL CRITICAL      │ svc-a -> svc-b    │
│ trace-2   │ [2] Redundant SQL WARNING │ ...               │
│ ...       │ [3] Slow HTTP INFO        │                   │
├───────────┴───────────────────────────┴───────────────────┤
│ Detail (largeur complète, arbre de spans + métadonnées)   │
└───────────────────────────────────────────────────────────┘
```

La bordure du panel actif est cyan, le reste reste gris.

## Keybindings

La navigation fonctionne aux flèches partout. Dans la vue Inspect, les
touches vim `h` / `j` / `k` / `l` s'appliquent aussi, et `j` / `k`
scrollent les vues Analyze et Explain.

| Touche                | Action                                                 |
|-----------------------|--------------------------------------------------------|
| `q`                   | Quitter                                                |
| `↑` / `k`             | Sélection vers le haut, ou scroll (Analyze, Explain)   |
| `↓` / `j`             | Sélection vers le bas, ou scroll (Analyze, Explain)    |
| `→` / `Tab` / `l`     | Cycle vers le panneau suivant (Inspect)                |
| `←` / `BackTab` / `h` | Cycle vers le panneau précédent (Inspect)              |
| `Enter`               | Descend d'un cran dans le drill-down (voir ci-dessous) |
| `Esc`                 | Remonte d'un cran                                      |
| `a`                   | Acknowledger le finding sélectionné (mode live)        |
| `u`                   | Révoquer l'ack existant (mode live)                    |

`Enter` descend : d'Analyze vers Inspect, puis à travers les panneaux
d'Inspect (Traces, Findings, Detail), puis de Detail vers Explain. Depuis
le panneau Correlations, il saute vers le Detail de la trace d'exemple de
la corrélation. `Esc` inverse chaque étape, remontant des panneaux de
tête d'Inspect jusqu'à Analyze.

`a` et `u` sont no-op en mode batch (`inspect --input`) puisque
l'acknowledgment a besoin d'un daemon qui tourne pour persister.

## Flow d'acknowledgment (mode live)

Quand le TUI est lancé via `query inspect`, il fetch les findings avec
`?include_acked=true` pour que les findings déjà acknowledged
apparaissent dans la liste avec un indicateur italique gris
`[acked by <user>]` à droite de la ligne.

### `a` : créer un ack

Presser `a` sur un finding sélectionné ouvre une modale centrée sur
l'écran avec trois champs d'input :

| Champ   | Contrainte                      | Default                 |
|---------|---------------------------------|-------------------------|
| Reason  | 1 à 256 chars, single-line      | vide (requis)           |
| Expires | vide, `24h`, `7d`, ISO8601, etc | vide (pas d'expiration) |
| By      | 1 à 128 chars                   | env var `$USER`         |

Plus deux boutons (`Submit` / `Cancel`).

Navigation modale :

| Touche           | Action                                       |
|------------------|----------------------------------------------|
| `Tab`            | Focus sur le champ ou bouton suivant         |
| `BackTab`        | Focus arrière                                |
| `Enter` (texte)  | Avance au champ suivant                      |
| `Enter` (Submit) | Envoie le formulaire                         |
| `Enter` (Cancel) | Ferme la modale sans envoyer                 |
| `Esc`            | Annule la modale                             |
| `Backspace`      | Supprime le dernier char du buffer focus     |

Au submit, le TUI poste sur `/api/findings/<sig>/ack` et ferme la
modale sur 201. En cas d'erreur (4xx/5xx), la modale reste ouverte
avec le message d'erreur en bas (texte rouge).

### `u` : révoquer un ack

Presser `u` sur un finding acknowledged ouvre une modale de
confirmation. `Submit` / `Enter` envoie un `DELETE
/api/findings/<sig>/ack`. `Cancel` / `Esc` ferme sans révoquer.

### Format expires

Mirror du CLI ack helper (depuis 0.5.22) :

- Vide : pas d'expiration, l'ack persiste jusqu'à revoke manuel
- `24h`, `7d`, `30m` : durée relative parsée par humantime
- `2026-05-11T00:00:00Z` : datetime ISO8601 absolu

Une entrée invalide affiche `expires: <erreur>` dans le footer de la
modale sans envoyer la requête.

## Authentification

Le TUI mirror la résolution d'auth du CLI ack helper :

1. Variable d'env `PERF_SENTINEL_DAEMON_API_KEY` (priorité 1)
2. Flag `--api-key-file <path>` sur `query inspect` (priorité 2)

```bash
# variable d'env
export PERF_SENTINEL_DAEMON_API_KEY=$(cat ~/.config/perf-sentinel/key)
perf-sentinel query --daemon http://localhost:4318 inspect

# fichier
perf-sentinel query --daemon http://localhost:4318 inspect \
  --api-key-file ~/.config/perf-sentinel/key
```

Les deux sont équivalents. Le path supporte le refus de symlink via
`O_NOFOLLOW` sur Unix et trim les newlines en fin de fichier.

**Pas de prompt password interactif dans le TUI.** Le raw mode et
l'alternate screen sont incompatibles avec l'input TTY de
`rpassword`. Si le daemon répond 401 sans clé env ou file, la modale
affiche "API key required: set PERF_SENTINEL_DAEMON_API_KEY or pass
--api-key-file when launching `query inspect`." Quitter, définir la clé,
relancer.

Quand le daemon n'a pas de `[daemon.ack] api_key` configuré (default
pour les déploiements loopback), aucune clé n'est requise et la
modale envoie directement.

## Caveats

### Le HTTP synchrone freeze l'UI

`run_loop` est synchrone et le write daemon ack est exécuté via
`tokio::runtime::Handle::current().block_on(...)` depuis l'intérieur
de la loop. L'UI freeze pour la durée de la requête, typiquement
100-300 ms en localhost, plus long sur le réseau. Acceptable pour une
release scope-minimal. Un refactor en async event loop est un
followup candidat si le feedback utilisateur signale de la friction.

### Snapshot de la liste des findings

La liste des findings est fetchée une seule fois au boot. `a`/`u`
rafraîchissent uniquement l'état des acks via un second `GET
/api/findings?include_acked=true`, la liste des findings elle-même ne
change pas en cours de session. Pour récupérer des traces nouvellement
ingérées, quitter et relancer.

### Acks TOML visibles, pas modifiables

Les findings ack dans `.perf-sentinel-acknowledgments.toml` (CI ack)
apparaissent avec l'indicateur `[acked by <user>]` et le champ source
positionné à `toml`. Le TUI ne peut pas promouvoir un ack daemon vers
TOML ni éditer le fichier TOML. Pour les acks permanents, éditer le
fichier via revue PR comme décrit dans
[`ACK-WORKFLOW-FR.md`](./ACK-WORKFLOW-FR.md).

## Voir aussi

- [`ACK-WORKFLOW-FR.md`](./ACK-WORKFLOW-FR.md) pour la relation entre
  les acks TOML CI et les acks JSONL daemon, plus la table de
  décision complète.
- [`CLI-FR.md`](./CLI-FR.md) pour la référence de la sous-commande
  `perf-sentinel ack` (équivalent CLI de `a`/`u`).
- [`HTML-REPORT-FR.md`](./HTML-REPORT-FR.md) pour le flow ack
  navigateur via `--daemon-url`.
- [`CONFIGURATION-FR.md`](./CONFIGURATION-FR.md) pour la référence
  config `[daemon.ack]` côté serveur.
