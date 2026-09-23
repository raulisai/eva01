#!/usr/bin/env python3
"""Lee `cargo metadata` por stdin y escribe THIRD_PARTY_NOTICES.md por stdout.
Lo llama packaging/third-party.sh."""
import json, sys

meta = json.load(sys.stdin)
workspace = set(meta["workspace_members"])
in_build = {node["id"] for node in meta["resolve"]["nodes"]}
BANNED = ("GPL", "AGPL", "SSPL", "BUSL")

crates, problems = [], []
for pkg in meta["packages"]:
    if pkg["id"] in workspace or pkg["id"] not in in_build:
        continue
    license = pkg.get("license") or ""
    if not license or any(word in license.upper() for word in BANNED):
        problems.append(f"{pkg["name"]} {pkg["version"]}: {license or "sin licencia"}")
    crates.append((pkg["name"], pkg["version"], license or "?", pkg.get("repository") or ""))

if problems:
    sys.exit("dependencias que no se pueden distribuir tal cual:\n  " + "\n  ".join(problems))

print("# Avisos de terceros")
print()
print("EVA01 es MIT (ver `LICENSE`). Incluye o descarga lo siguiente. Generado por")
print("`packaging/third-party.sh`; no lo edites a mano.")
print()
print("## Modelos de voz")
print()
print("`eva model install` descarga, desde Hugging Face, conversiones a ONNX de modelos de")
print("NVIDIA, bajo la licencia [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/):")
print()
print("- Canary 1B Flash y Canary 180M Flash — © NVIDIA; conversión a ONNX (int8) de")
print("  [istupakov](https://huggingface.co/istupakov).")
print("- El preprocesador `nemo128.onnx` — de `parakeet-tdt-0.6b-v3` (© NVIDIA, CC BY 4.0),")
print("  conversión de istupakov.")
print()
print("Los modelos no se distribuyen con EVA01: se descargan al instalarlos.")
print()
print(f"## Dependencias de Rust ({len(crates)})")
print()
print("| Crate | Versión | Licencia |")
print("|---|---|---|")
for name, version, license, repo in sorted(crates):
    label = f"[{name}]({repo})" if repo else name
    print(f"| {label} | {version} | {license} |")
