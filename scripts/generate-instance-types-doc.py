#!/usr/bin/env python3
"""Generate the instance-type reference doc from the embedded power table.

The table is itself generated (`scripts/refresh-instance-power.py`), so the
doc is generated too rather than transcribed: 356 rows copied by hand would
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
    """Classify the naming schemes used by the embedded table."""
    if name.startswith("xeon-"):
        return "Bare metal"
    if name.startswith("Standard_"):
        return "Azure"
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
    providers = ("AWS", "GCP", "Azure", "Bare metal")
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

**Only AWS, GCP and Azure families are listed**, and `[green.cloud]`
accepts only those three providers. The wattages come from the Cloud
Carbon Footprint coefficient CSVs, which publish nothing for OVHcloud,
Scaleway or OUTSCALE. Those three carry grid intensity and PUE in the
carbon table, so their regions score, but no published figure supports a
per-instance power profile for them, and a made-up wattage would not be
an estimate. Searched, as of August 2026:

- **Boavizta** ([BoaviztAPI](https://github.com/Boavizta/boaviztapi)) is
  the only third-party base that reaches instance granularity, with 50
  OVHcloud sizes and 69 Scaleway ones. Its files carry **no power
  column** at all, and for OVHcloud the CPU those wattages would be
  derived from is Boavizta's own assumption, flagged `CPU not verified`
  on all 12 archetype rows. One credits a rack server with an
  `Intel Core i7-4940MX`, a mobile part, across two sockets.
- **Scaleway** publishes only `kg_co2_equivalent` and `m3_water_usage`,
  account-scoped, and states that instead of measuring instance power it
  feeds a CPU-percentage proxy into a Boavizta consumption profile. Its
  Product Catalog API does expose CPU sockets, cores and frequency per
  SKU, which is a lead, but no wattage.
- **OUTSCALE** stops at (Region, service category): two regions, three
  categories, no instance type, no watt, no kWh.

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

**Seules les familles AWS, GCP et Azure sont listées**, et
`[green.cloud]` n'accepte que ces trois fournisseurs. Les puissances
proviennent des CSV de coefficients Cloud Carbon Footprint, qui ne
publient rien pour OVHcloud, Scaleway ou OUTSCALE. Ces trois
fournisseurs portent une intensité réseau et un PUE dans la table
carbone, leurs régions sont donc scorées, mais aucun chiffre publié ne
soutient un profil de puissance par instance pour eux, et une puissance
inventée ne serait pas une estimation. Recherché, en août 2026 :

- **Boavizta** ([BoaviztAPI](https://github.com/Boavizta/boaviztapi))
  est la seule base tierce à descendre au type d'instance, avec 50
  tailles OVHcloud et 69 Scaleway. Ses fichiers ne portent **aucune
  colonne de puissance**, et pour OVHcloud le CPU dont ces puissances
  seraient dérivées est une hypothèse de Boavizta lui-même, marquée
  `CPU not verified` sur les 12 lignes d'archétype. L'une crédite un
  serveur rack d'un `Intel Core i7-4940MX`, une puce mobile, sur deux
  sockets.
- **Scaleway** ne publie que `kg_co2_equivalent` et `m3_water_usage`,
  limités au compte appelant, et déclare qu'au lieu de mesurer la
  puissance d'une instance il injecte un proxy de pourcentage CPU dans
  un profil de consommation Boavizta. Son API Product Catalog expose
  bien les sockets, cœurs et fréquence par SKU, ce qui est une piste,
  mais aucune puissance.
- **OUTSCALE** s'arrête au couple (Région, catégorie de service) : deux
  régions, trois catégories, aucun type d'instance, aucun watt, aucun
  kWh.

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
