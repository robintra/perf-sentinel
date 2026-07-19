# Understanding the energy and carbon figures

This page tells the whole energy story in plain language: what perf-sentinel counts, what it measures, how a number of I/O operations becomes kilowatt-hours and grams of CO2, and what changes depending on which options you enable. It is a synthesis, not a reference. The formulas live in [METHODOLOGY.md](METHODOLOGY.md), the precision bounds in [LIMITATIONS.md](LIMITATIONS.md), and every configuration key in [CONFIGURATION.md](CONFIGURATION.md).

## The idea in one paragraph

perf-sentinel reads distributed traces and counts every I/O operation an application performs: SQL queries and outbound HTTP calls. Its detectors flag the operations that did not need to happen, mainly N+1 loops and redundant repeated calls. The ratio between avoidable and total operations is the waste ratio, and it is the most robust number the tool produces because it does not depend on any energy model at all. Everything else in this page is about turning the operation counts into energy and carbon: the waste ratio tells you which share is wasted, the energy pipeline tells you how much that share weighs in kWh and gCO2.

## Where each number comes from

The carbon figures follow the Software Carbon Intensity model (SCI, standardized as ISO/IEC 21031:2024): carbon = energy x grid intensity, plus an embodied term for the hardware itself.

- **Energy (E)** starts as an estimate and becomes a measurement as you plug in backends. With nothing configured, every operation costs a fixed `1e-7 kWh` (the proxy coefficient, model tag `io_proxy_v3`). Each backend below replaces that estimate with something closer to physical reality.
- **Grid intensity (I)** converts kWh into gCO2 for the region where the code runs. It starts from embedded annual national averages, can be modulated by 24-hour profiles, and becomes a live value when the Electricity Maps API is configured. The region itself comes from the `cloud.region` span attribute, from `[green.service_regions]`, or from `[green] default_region`, in that order.
- **Embodied carbon (M)** accounts for manufacturing the servers. It is a fixed per-request figure derived from public lifecycle assessments of rack servers (Boavizta and the Cloud Carbon Footprint methodology), configurable via `embodied_carbon_per_request_gco2`. Fixing an N+1 does not un-manufacture silicon, so the avoidable figures never include this term.
- **PUE** multiplies the energy to account for datacenter overhead (cooling, power distribution): 1.09 for GCP, 1.15 for AWS, 1.17 for Azure, 1.5 for unknown infrastructure.

Every report states which model produced its numbers through the `energy_model` and `per_service_energy_model` tags, so a reader can always tell an estimate from a measurement.

## The fidelity ladder

Each row of this table replaces or refines the row above it. You can stop at any rung and the reports stay honest about which rung you are on.

| You configure                         | What you get                                                                                                                                                                                                   | Model tag                    |
|---------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|------------------------------|
| Nothing (default)                     | Fixed `1e-7 kWh` per operation, weighted by SQL verb (SELECT 0.5x, INSERT/UPDATE 1.5x, DELETE 1.2x) and HTTP payload size tiers. Directional, order-of-magnitude, carries a 2x uncertainty bracket.            | `io_proxy_v3`                |
| `calibrate` from a measured power CSV | The proxy coefficient is rescaled per service from your own measured joules per operation. Still a model, but anchored to your hardware.                                                                       | `io_proxy_*+cal`             |
| `[green.cloud]`                       | Cloud VM energy interpolated from CPU utilization and the public SPECpower database of server power curves, following the Cloud Carbon Footprint methodology.                                                  | `cloud_specpower`            |
| `[green.redfish]`                     | Wall-plug power read from the server's BMC. The only backend that sees fans, disks and power-supply overhead. Bare metal only.                                                                                 | `redfish_bmc`                |
| `[green.kepler]`                      | Kepler's per-container estimates. Ranked below the RAPL backends because independent evaluation measured large attribution errors, see the sources.                                                            | `kepler_ebpf`                |
| `[green.scaphandre]`                  | CPU energy from Intel RAPL counters, attributed per process by Scaphandre.                                                                                                                                     | `scaphandre_rapl`            |
| `[green.alumet]`                      | CPU energy from RAPL, attributed per cgroup by Alumet. The recommended measured backend: same counters as Scaphandre, sampling characterized as less error-prone by its authors, container-shaped attribution. | `alumet_rapl`                |
| `[green.electricity_maps]`            | Does not change E. Replaces the annual grid intensity with the live value for your region, which is the largest lever on the gCO2 figures in regions with variable electricity mixes.                          | intensity source `real_time` |

When several backends cover the same service, the daemon keeps the highest-fidelity reading: `alumet_rapl` beats `scaphandre_rapl`, which beats `kepler_ebpf`, then `redfish_bmc`, then `cloud_specpower`, then the proxy. All measured backends are daemon-only (`watch`), batch `analyze` always uses the proxy path.

One honest constraint applies to every RAPL-based rung: the hardware counters only see CPU and DRAM, which is roughly half to two thirds of what the server draws at the wall. Only Redfish sees the rest.

## The database figure

Counting operations on the application side misses a structural point: the energy of an N+1 is mostly burned by the database that executes the N queries, and a database emits no spans, so it is invisible to the per-service attribution. The `[green.alumet.database]` declaration closes that gap with a deliberately simple rule of three: point Alumet at the database cgroup, and each scoring window multiplies the measured database energy by the SQL-only waste ratio.

```
database waste = measured DB energy x (avoidable SQL ops / total SQL ops)
```

The result is `green_summary.database_waste`, with a gCO2 conversion when you declare the database's region. And the figure exists even without Alumet: when no measurement is available (batch runs, managed databases, no `[green.alumet.database]`), it is estimated from the modeled energy of the SQL spans instead, and its `model` tag says which path produced it (`alumet_rapl` = measured, `estimated` = modeled). Measured is a lower bound (CPU energy only, no DRAM, no disk), estimated inherits the proxy's 2x bracket, both use a count-based ratio, so the figure stays informational: the measured variant is additional energy excluded from `energy_kwh` and `co2`, the estimated variant is a re-presented share of those totals (never add it on top), and the disclosure publishes it only as a separate labeled block outside every total. The full list of bounds is in [LIMITATIONS.md](LIMITATIONS.md#alumet-precision-bounds).

## What the numbers are not

The tool is a directional waste counter with increasingly good energy anchoring, not a wattmeter and not a certified carbon inventory. The proxy path carries a 2x multiplicative bracket. Idle and static server power is not redistributed to services. The count-based ratios treat a cheap indexed SELECT and a heavy write the same, which academic measurements show can differ by tens of percent in power terms. All of this is quantified, with the reasoning, in [LIMITATIONS.md](LIMITATIONS.md).

## Sources

What each external source contributes to the numbers above:

- **Green Software Foundation, Software Carbon Intensity specification (ISO/IEC 21031:2024)**: the `carbon = E x I + M` frame, the location-based grid intensity requirement, and the obligation to disclose the methodology behind every figure.
- **Tsirogiannis, Harizopoulos, Shah, "Analyzing the Energy Efficiency of a Database Server", SIGMOD 2010**: database operators at the same CPU utilization can differ by up to 60% in power. Grounds the SQL verb multipliers and the honesty caveat on count-based ratios.
- **Xu, Tu, Wang, "Exploring Power-Performance Tradeoffs in Database Systems", ICDE 2010** and **Lella et al., "DBJoules: An Energy Measurement Tool for Database Management Systems", arXiv:2311.08961**: the relative energy cost of SQL operation classes behind the per-verb weighting.
- **Khan et al., "RAPL in Action: Experiences in Using RAPL for Power Measurements", ACM TOMPECS 2018**: RAPL readings correlate closely with external plug-power measurements, which is why RAPL-based backends rank above model-based ones.
- **Raffin, Trystram, "Dissecting the software-based measurement of CPU energy consumption: a comparative analysis", arXiv:2401.15985 (IEEE TPDS 2025)**: the pitfalls of software RAPL readers, and the reason `alumet_rapl` outranks `scaphandre_rapl`.
- **Raffin, Trystram, Richard, "Alumet: a Modular Framework to Standardize the Measurement of Energy Consumption", PECS 2025**: the measurement framework behind the recommended backend.
- **Pijnacker et al., "Container-level Energy Observability in Kubernetes Clusters", arXiv:2504.10702** and the **CNCF post "Kepler, re-architected" (June 2026)**: independently measured attribution errors in Kepler's eBPF model and the upstream redesign that followed, the reason `kepler_ebpf` sits below the RAPL backends.
- **Mytton, Lunden, Malmodin, "Network energy use not directly proportional to data volume", Journal of Industrial Ecology 28(4), 2024**: why the optional network transport term is modeled conservatively.
- **SPEC SPECpower_ssj2008 published results and the Cloud Carbon Footprint methodology**: the utilization-to-watts interpolation behind `cloud_specpower`.
- **Boavizta**: the server lifecycle assessments behind the embodied term.
- **Electricity Maps**: the live grid intensity API behind the `real_time` intensity source.

The deeper design rationale, including how each backend's reading shape is normalized, lives in `docs/design/05-GREENOPS-AND-CARBON.md`.
