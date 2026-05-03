# Acquittements (acknowledgments)

Une façon de dire à perf-sentinel "oui, ce finding est réel, et nous avons décidé de ne pas le corriger (pour l'instant)". Les findings acquittés sont retirés de la sortie CLI et exclus de la quality gate. Les décisions vivent dans `.perf-sentinel-acknowledgments.toml` à la racine du repo, donc chaque modification passe par la review PR habituelle et `git log` est la trace d'audit.

Ce document couvre le format, le workflow, les flags CLI et la FAQ.

## Quand l'utiliser

- L'équipe a décidé qu'un finding est intentionnel (pattern d'invalidation de cache, batch volontaire, script jetable avec O(N) appels).
- Un workaround long terme tracé ailleurs (Jira, ADR) que vous ne voulez plus voir flaggé sur chaque CI tant que la cause racine n'est pas résolue.
- Un finding qui flappait sous une charge bruyante, et que l'équipe a décidé de revisiter quand le problème upstream sera résolu.

Si vous hésitez, ne l'acquittez **PAS**. Chaque ack masque un signal réel. Le seuil doit être : "on en a discuté, on a décidé".

## Le fichier

Chemin : `./.perf-sentinel-acknowledgments.toml` à la racine du repo où vous lancez `perf-sentinel`. Override avec `--acknowledgments <chemin>`.

```toml
# .perf-sentinel-acknowledgments.toml
#
# Ce fichier documente les findings perf-sentinel acquittés par
# l'équipe comme connus et intentionnels. Les findings acquittés sont
# retirés de la sortie CLI (analyze, report, inspect, diff) et ne
# pèsent plus sur la quality gate.
#
# Chaque entry est matchée contre la signature du finding, calculée
# comme :
#   <finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix-of-template>
#
# Pour récupérer la signature d'un finding :
#   perf-sentinel analyze --input traces.json --format json | jq '.findings[].signature'

[[acknowledged]]
signature = "redundant_sql:order-service:POST__api_orders:cafebabecafebabe"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-05-02"
reason = "Pattern d'invalidation de cache, intentionnel. Voir ADR-0042."
expires_at = "2026-12-31"  # Optionnel, omettre pour rendre l'ack permanent.

[[acknowledged]]
signature = "slow_sql:report-service:GET__api_reports:deadbeefdeadbeef"
acknowledged_by = "bob@example.com"
acknowledged_at = "2026-04-15"
reason = "Agrégation longue, accepté par le produit."
# Pas d'expires_at : ack permanent.
```

### Référence des champs

| Champ             | Requis | Notes                                                                              |
|-------------------|--------|------------------------------------------------------------------------------------|
| `signature`       | oui    | Signature canonique du finding (voir plus bas).                                    |
| `acknowledged_by` | oui    | Email ou identifiant. Texte libre.                                                 |
| `acknowledged_at` | oui    | Date ISO 8601 `YYYY-MM-DD`. Texte libre, non validé.                               |
| `reason`          | oui    | Texte libre. Court, avec lien vers ADR / Jira / thread Slack.                      |
| `expires_at`      | non    | Date ISO 8601 `YYYY-MM-DD`. Validée au chargement. Omettre pour un ack permanent.  |

Un champ requis manquant fait échouer le run avec une erreur claire, donc une coquille n'élargit pas silencieusement le set acquitté.

## Format de signature

```
<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix-of-template>
```

- `finding_type` est l'enum snake_case : `n_plus_one_sql`, `redundant_sql`, `slow_http`, `chatty_service`, etc.
- `service` est le nom de service OpenTelemetry tel que capturé dans la trace (e.g. `order-service`).
- `sanitized_endpoint` est `source_endpoint` avec `/` et espaces remplacés par `_` pour que le résultat se split proprement sur `:`.
- `sha256-prefix-of-template` correspond aux 16 premiers chars hex (8 octets) de `sha256(pattern.template)`. ~64 bits de résistance aux collisions. Comme le triplet `(finding_type, service, sanitized_endpoint)` fait déjà partie de la signature, le hash n'a besoin de désambiguer que les templates au sein du même triplet, ce qui est une population très réduite en pratique. Le préfixe 16-char est une défense en profondeur contre le masquage accidentel d'un ack après un refacto SQL ou un renommage de service.

Trois findings produisent trois signatures différentes. Deux findings produits par le même template sur le même couple `(service, source_endpoint)` collapsent à la même signature, ce qui est la bonne sémantique : on ack une fois, on supprime chaque récurrence.

## Workflow

1. Lancez perf-sentinel et identifiez le finding à acquitter.
2. Capturez sa signature :
   ```bash
   perf-sentinel analyze --input traces.json --format json \
     | jq -r '.findings[] | select(.service == "order-service") | .signature'
   ```
3. Ouvrez une PR qui ajoute un bloc `[[acknowledged]]` à `.perf-sentinel-acknowledgments.toml`. Discutez le `reason` en review PR.
4. Mergez. Le run CI suivant lit le fichier mis à jour et le finding cesse d'apparaître.

`git log .perf-sentinel-acknowledgments.toml` donne l'historique d'audit complet.

## Flags CLI

Les flags fonctionnent uniformément sur `analyze`, `report`, `inspect`, `diff`.

| Flag                          | Effet                                                                                                                            |
|-------------------------------|----------------------------------------------------------------------------------------------------------------------------------|
| (par défaut, sans flag)       | Charge `./.perf-sentinel-acknowledgments.toml` s'il existe, l'applique. Pas de fichier = no-op, comportement actuel préservé.    |
| `--acknowledgments <chemin>`  | Override le chemin par défaut. Utile en monorepo avec un fichier d'acks par dossier de service.                                  |
| `--no-acknowledgments`        | Désactive le filtrage complètement. Pour les vues d'audit ("montre-moi tout, y compris ce que j'ai acquitté").                   |
| `--show-acknowledged`         | Applique le filtrage, mais inclut les findings acquittés dans la sortie avec leur metadata d'ack. Pour la review périodique.     |

## Comportement de la quality gate

Les findings acquittés sont exclus du calcul de la quality gate. Autrement dit : un finding qui aurait fait échouer `n_plus_one_sql_critical_max = 0` devient un PASS une fois acquitté.

C'est tout l'intérêt de la sémantique "won't fix / accepté". Si vous ne voulez pas ce comportement, n'acquittez pas le finding, abaissez le seuil, ou utilisez `--no-acknowledgments` en CI.

## Et la règle `io_waste_ratio_max` ?

La règle `io_waste_ratio_max` lit `green_summary.io_waste_ratio`, calculé depuis les spans bruts, pas depuis la liste de findings. Acquitter un finding N+1 ne baisse **pas** le waste ratio, parce que les opérations I/O sous-jacentes sont toujours réelles et toujours exécutées.

Décision : c'est le bon comportement. Un ack signifie "l'équipe a accepté ce finding, ne le flagge pas". Il ne signifie pas "fais comme si l'I/O n'avait pas lieu". Les chiffres carbone et waste sont une comptabilité honnête, l'ack contrôle le routing d'alerte.

## FAQ

**Q : Comment faire passer un ack temporaire en permanent ?**
Retirez la ligne `expires_at` et recommittez. La review PR capture la décision.

**Q : Comment debug un ack qui ne match pas ?**
Lancez `perf-sentinel analyze --no-acknowledgments --format json | jq '.findings[].signature'`, comparez la valeur à celle du fichier TOML. Causes courantes : le template s'est normalisé différemment après un changement de code, le nom de service a changé, la route endpoint a été renommée.

**Q : Puis-je acquitter un finding par service ou par type, avec des wildcards ?**
Non, le matching par signature exacte est intentionnel en 0.5.17. Les wildcards rendent trop facile le silence accidentel de catégories entières de findings. Si vous voulez acquitter 10 findings N+1 sur un service, ouvrez 10 PRs (ou une PR avec 10 entries), une signature chacune.

**Q : Et si je commit un ack qui s'avère incorrect ?**
Revertez le commit. Le run CI suivant fera réapparaître le finding.

**Q : Y a-t-il une API d'acknowledgments sur le daemon ?**
Pas en 0.5.17. Le chemin daemon est dans la roadmap (différé à une release ultérieure en attente de review architecture), le chemin CI/batch couvre la majorité des cas.

**Q : `inspect` (TUI) honore-t-il les acknowledgments ?**
Oui, les mêmes flags s'appliquent. La TUI n'a pas encore de panneau dédié pour les findings supprimés, mais le footer de status surface le count.

**Q : Le dashboard HTML surface-t-il la metadata d'ack ?**
Avec `--show-acknowledged`, le payload JSON embarqué inclut le tableau `acknowledged_findings` (visible dans DevTools ou avec `jq` sur la donnée embarquée). L'UI visuelle n'a pas encore de section dédiée aux acks, c'est dans la roadmap dashboard.

## Intégration SARIF

Depuis 0.5.18, l'emitter SARIF expose la signature du finding à deux endroits, pour que les outils CI qui consomment du SARIF (GitHub Code Scanning, GitLab SAST, Sonar) puissent matcher les findings contre `.perf-sentinel-acknowledgments.toml` sans avoir à parser séparément le JSON.

- `runs[].results[].properties.signature` porte la chaîne de signature canonique, cohérent avec les autres champs ack déjà présents dans `properties` (`acknowledged`, `acknowledgmentReason`, ...).
- `runs[].results[].fingerprints["perfsentinel/v1"]` expose la même valeur via le mécanisme natif `fingerprints` de SARIF v2.1.0 (section 3.27.17), utilisé par GitHub Code Scanning et GitLab SAST pour la déduplication cross-run.

Les deux champs portent la même valeur, à choisir selon le modèle d'ingestion de l'outil. Les findings désérialisés à partir de baselines produites avant 0.5.17 ont une signature vide, et l'emitter SARIF omet les deux champs dans ce cas (graceful degradation).

Voir [`SARIF-FR.md`](SARIF-FR.md) pour la référence complète des champs émis par result.

## Références croisées

- [`README-FR.md`](../../README-FR.md) section "Acquitter les findings connus" pour le pitch rapide.
- [`CONFIGURATION-FR.md`](CONFIGURATION-FR.md) pour l'interaction entre `.perf-sentinel.toml` et `.perf-sentinel-acknowledgments.toml`.
- [`RUNBOOK-FR.md`](RUNBOOK-FR.md) section "Investigation d'un acknowledgment inattendu" pour la recette d'astreinte.
