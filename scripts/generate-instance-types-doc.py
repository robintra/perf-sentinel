#!/usr/bin/env python3
"""Generate the instance-type reference doc from the embedded power table.

The table is itself generated (`scripts/refresh-instance-power.py`), so the
doc is generated too rather than transcribed: 400+ rows copied by hand would
drift the first time the table is refreshed. `instance_types_doc_matches_the_table`
in `cloud_energy/table.rs` fails the build when the two disagree.

Usage: python3 scripts/generate-instance-types-doc.py
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SOURCES = [
    ROOT / "crates/sentinel-core/src/score/cloud_energy/table_data.rs",
    ROOT / "crates/sentinel-core/src/score/cloud_energy/table.rs",
]
ROW = re.compile(r'^\s*\("([^"]+)",\s*([0-9.]+),\s*([0-9.]+)\)', re.M)
VINTAGE = re.compile(r'SPECPOWER_VINTAGE: &str = "([^"]+)"')


def provider(name):
    """Classify the naming schemes used by the embedded table.

    Scaleway offer ids are the only all-upper-case, dash-separated ones
    (`COMPUTE3-X2C-4G`, `POP2-HM-2C-16G`), which the GCP fallback at the
    bottom would otherwise swallow whole. Matched on that shape rather
    than on a list of range prefixes: the offer list is read live from
    the Product Catalog by `refresh-instance-power.py`, so a new range
    lands in the table without anyone editing this file, and a prefix
    list would file it under GCP silently.
    """
    if name.startswith("xeon-"):
        return "Bare metal"
    if name.startswith("Standard_"):
        return "Azure"
    if "-" in name and "." not in name and name == name.upper():
        return "Scaleway"
    if "." in name:
        return "AWS"
    return "GCP"


def collect():
    rows, seen = [], set()
    for path in SOURCES:
        for name, idle, mx in ROW.findall(path.read_text()):
            if name in seen:
                continue
            seen.add(name)
            rows.append((name, float(idle), float(mx)))
    return rows


def table(rows, headers):
    out = [f"| {headers[0]} | {headers[1]} | {headers[2]} |", "|---|---:|---:|"]
    for name, idle, mx in sorted(rows):
        out.append(f"| `{name}` | {idle:g} | {mx:g} |")
    return "\n".join(out)


def render(rows, vintage, lang):
    providers = ("AWS", "GCP", "Azure", "Scaleway", "Bare metal")
    by_provider = {p: [r for r in rows if provider(r[0]) == p] for p in providers}
    if lang == "en":
        head = f"""# Instance types with an embedded power profile

Every `instance_type` accepted by `[green.cloud.services]` and by
`[green.broker_static]`, with the idle and maximum wattage the SPECpower
model interpolates between. {len(rows)} entries, table vintage
`{vintage}`.

**An unlisted type is not an error.** perf-sentinel warns once at startup,
naming the type, and falls back to a provider-level average: the figure
gets coarser, nothing breaks. That warning is also how you check your own
type without reading this page. When your hardware is absent and you know
its draw, declare it directly instead, which is exact rather than
approximated:

```toml
[green.cloud.services]
"my-service" = {{ idle_watts = 45.0, max_watts = 120.0 }}
```

**Scaleway rows are derived, not published.** Cloud Carbon Footprint
publishes coefficients for AWS, GCP and Azure only. Scaleway publishes
no wattage either, but its Product Catalog API names the **exact CPU** of
every offer without authentication (`AMD EPYC 7543`, not a generic
family), so each offer is priced by the CCF coefficient for that CPU's
architecture, the same vCPU-times-coefficient arithmetic as the three
above. What that adds is one assumption, the same CCF already makes
between AWS and GCP: that a coefficient computed on hyperscaler fleets
transfers to comparable silicon elsewhere. Three groups of offers are
excluded rather than approximated:

- **shared-vCPU ranges** (`DEV1`, `PLAY2`, `BASIC2`, `BASIC3`), where
  attributing a whole vCPU to one tenant overstates what it draws;
- **GPU offers** (`H100`, `L4`, `L40S`, `RENDER`), because the table
  models no accelerator for any provider, and an H100 alone outweighs
  the entire CPU budget;
- **AmpereOne and Granite Rapids ranges** (`STANDARD2`, `B300`), absent
  from the CCF CSVs.

**OVHcloud and OUTSCALE are still absent**, and not for want of looking.
Searched, as of August 2026:

- **Boavizta** ([BoaviztAPI](https://github.com/Boavizta/boaviztapi)) is
  the only third-party base that reaches instance granularity, with 50
  OVHcloud sizes. Its files carry **no power column** at all, and the
  CPU a wattage would be derived from is Boavizta's own assumption,
  flagged `CPU not verified` on all 12 OVHcloud archetype rows. One
  credits a rack server with an `Intel Core i7-4940MX`, a mobile part,
  across two sockets. OVHcloud itself documents no CPU per range.
- **OUTSCALE** stops at (Region, service category): two regions, three
  categories, no instance type, no watt, no kWh, and zero occurrences in
  Boavizta.

On that hardware, measure instead: Alumet or Scaphandre read RAPL
directly and outrank every modeled figure.

Where the numbers come from, and why a family maps to a coefficient
rather than to a measured machine: [`METHODOLOGY.md`](./METHODOLOGY.md)
and `docs/design/05-GREENOPS-AND-CARBON.md`. Configuring the scraper:
[`CONFIGURATION.md`](./CONFIGURATION.md).

This page is generated from the embedded table by
`scripts/generate-instance-types-doc.py`, and a test fails the build if
the two ever disagree. Do not edit it by hand.
"""
        headers = ("Instance type", "Idle (W)", "Max (W)")
        units = ("entry", "entries")
    else:
        head = f"""# Types d'instance avec un profil de puissance embarqué

Tous les `instance_type` acceptés par `[green.cloud.services]` et par
`[green.broker_static]`, avec les puissances au repos et maximale entre
lesquelles le modèle SPECpower interpole. {len(rows)} entrées, millésime
de la table `{vintage}`.

**Un type absent n'est pas une erreur.** perf-sentinel émet un
avertissement au démarrage en le nommant, puis retombe sur une moyenne du
fournisseur : la valeur devient plus grossière, rien ne casse. Cet
avertissement est aussi la façon de vérifier votre propre type sans lire
cette page. Quand votre matériel est absent et que vous connaissez sa
consommation, déclarez-la directement, c'est exact plutôt qu'approché :

```toml
[green.cloud.services]
"mon-service" = {{ idle_watts = 45.0, max_watts = 120.0 }}
```

**Les lignes Scaleway sont dérivées, pas publiées.** Cloud Carbon
Footprint ne publie des coefficients que pour AWS, GCP et Azure.
Scaleway ne publie pas non plus de puissance, mais son API Product
Catalog nomme le **CPU exact** de chaque offre sans authentification
(`AMD EPYC 7543`, et non une famille générique) : chaque offre est donc
valorisée par le coefficient CCF de l'architecture de ce CPU, selon la
même arithmétique vCPU fois coefficient que les trois autres. Cela
ajoute une seule hypothèse, celle que CCF pose déjà entre AWS et GCP :
qu'un coefficient calculé sur des flottes d'hyperscalers se transpose à
du silicium comparable ailleurs. Trois groupes d'offres sont exclus
plutôt qu'approchés :

- les **gammes à vCPU partagés** (`DEV1`, `PLAY2`, `BASIC2`, `BASIC3`),
  où attribuer un vCPU entier à un locataire surestime ce qu'il tire ;
- les **offres GPU** (`H100`, `L4`, `L40S`, `RENDER`), parce que la
  table ne modélise aucun accélérateur chez aucun fournisseur, et
  qu'une H100 pèse à elle seule plus que tout le budget CPU ;
- les **gammes AmpereOne et Granite Rapids** (`STANDARD2`, `B300`),
  absentes des CSV de CCF.

**OVHcloud et OUTSCALE restent absents**, et ce n'est pas faute d'avoir
cherché. Recherché, en août 2026 :

- **Boavizta** ([BoaviztAPI](https://github.com/Boavizta/boaviztapi))
  est la seule base tierce à descendre au type d'instance, avec 50
  tailles OVHcloud. Ses fichiers ne portent **aucune colonne de
  puissance**, et le CPU dont une puissance serait dérivée est une
  hypothèse de Boavizta lui-même, marquée `CPU not verified` sur les 12
  lignes d'archétype OVHcloud. L'une crédite un serveur rack d'un
  `Intel Core i7-4940MX`, une puce mobile, sur deux sockets. OVHcloud
  lui-même ne documente aucun CPU par gamme.
- **OUTSCALE** s'arrête au couple (Région, catégorie de service) : deux
  régions, trois catégories, aucun type d'instance, aucun watt, aucun
  kWh, et zéro occurrence chez Boavizta.

Sur ce matériel, mesurez plutôt : Alumet ou Scaphandre lisent RAPL
directement et priment sur toute valeur modélisée.

D'où viennent ces chiffres, et pourquoi une famille correspond à un
coefficient plutôt qu'à une machine mesurée :
[`METHODOLOGY-FR.md`](./METHODOLOGY-FR.md) et
`docs/design/05-GREENOPS-AND-CARBON.md`. Configurer le scraper :
[`CONFIGURATION-FR.md`](./CONFIGURATION-FR.md).

Cette page est générée depuis la table embarquée par
`scripts/generate-instance-types-doc.py`, et un test casse le build si les
deux divergent. Ne la modifiez pas à la main.
"""
        headers = ("Type d'instance", "Repos (W)", "Max (W)")
        units = ("entrée", "entrées")

    parts = [head]
    for name, entries in by_provider.items():
        count = len(entries)
        parts.append(f"## {name} ({count} {units[count != 1]})")
        parts.append("")
        parts.append(table(entries, headers))
        parts.append("")
    return "\n".join(parts)


def main():
    rows = collect()
    vintage = VINTAGE.search(SOURCES[0].read_text())
    if not vintage:
        print("could not read SPECPOWER_VINTAGE", file=sys.stderr)
        return 1
    for lang, path in (
        ("en", ROOT / "docs/INSTANCE-TYPES.md"),
        ("fr", ROOT / "docs/FR/INSTANCE-TYPES-FR.md"),
    ):
        path.write_text(render(rows, vintage.group(1), lang))
        print(f"wrote {path.relative_to(ROOT)} ({len(rows)} rows)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
