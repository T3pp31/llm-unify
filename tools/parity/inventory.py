#!/usr/bin/env python3
"""Build a deterministic discovery inventory from the pinned source, without importing it.

Discovery is deliberately not a claim of OSS eligibility or functional coverage. Every
candidate needs a reviewed contract, scope decision, implementation and evidence.
"""
import argparse
import ast
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import sys

from source import ROOT, baseline, verify


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def expression(node):
    return ast.unparse(node) if node is not None else None


def literal(node):
    try:
        return ast.literal_eval(node)
    except (ValueError, TypeError):
        return None


def generate(config):
    source = ROOT / config["destination"]
    manifest = json.loads((source / "SOURCE.json").read_text())
    features = {}

    def add(kind, path, line, name, **details):
        identifier = f"{kind}:{path}:{name}"
        if identifier in features:
            return
        features[identifier] = {
            "id": identifier, "kind": kind, "name": name,
            "upstream": {"path": path, "line": line, "sha256": manifest["files"][path]},
            "scope": "unreviewed", "contract": {"input": None, "output": None},
            "status": "unimplemented", "verification_cases": [], **details,
        }

    parsed = {}
    for name in manifest["files"]:
        if name.startswith(("litellm/", "litellm-proxy-extras/")) and name.endswith(".py"):
            try:
                parsed[name] = ast.parse((source / name).read_text(encoding="utf-8"))
            except SyntaxError as error:
                # Never silently omit new syntax or a dynamically generated surface.
                add("discovery_error", name, error.lineno, "python_syntax", error=str(error))

    for name, tree in parsed.items():
        if name.startswith(("litellm/proxy/", "litellm-proxy-extras/")):
            for node in ast.walk(tree):
                if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    continue
                for decorator in node.decorator_list:
                    if not isinstance(decorator, ast.Call) or not isinstance(decorator.func, ast.Attribute):
                        continue
                    method = decorator.func.attr
                    if method not in {"get", "post", "put", "patch", "delete", "options", "head", "websocket", "api_route", "route"}:
                        continue
                    route = decorator.args[0] if decorator.args else next((k.value for k in decorator.keywords if k.arg in {"path", "route"}), None)
                    add("http", name, decorator.lineno, f"{method.upper()} {expression(route)} @{node.name}",
                        method=method.upper(), path_expression=expression(route),
                        path_literal=literal(route), handler=node.name,
                        signature=expression(node.args), response_annotation=expression(node.returns),
                        decorator=expression(decorator))
        if name == "litellm/types/utils.py":
            for node in tree.body:
                if isinstance(node, ast.ClassDef) and node.name == "LlmProviders":
                    for entry in node.body:
                        if isinstance(entry, ast.Assign) and isinstance(literal(entry.value), str):
                            add("provider", name, entry.lineno, literal(entry.value))
        if name == "litellm/_lazy_imports_registry.py":
            for node in tree.body:
                if isinstance(node, (ast.Assign, ast.AnnAssign)):
                    value = literal(node.value)
                    if isinstance(value, (list, tuple, dict)):
                        for export in value:
                            if isinstance(export, str) and not export.startswith("_"):
                                add("sdk_export", name, node.lineno, export)
        if name == "litellm/__init__.py":
            for node in tree.body:
                if isinstance(node, (ast.ImportFrom, ast.Import)):
                    for alias in node.names:
                        export = alias.asname or alias.name
                        if not export.startswith("_"):
                            add("sdk_export", name, node.lineno, export, module=getattr(node, "module", None))
                elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name) and not node.target.id.startswith("_"):
                    add("sdk_setting", name, node.lineno, node.target.id, annotation=expression(node.annotation))
        if name in {"litellm/proxy/_types.py", "litellm/types/router.py", "litellm/types/caching.py"}:
            for node in tree.body:
                if isinstance(node, ast.ClassDef):
                    for field in node.body:
                        if isinstance(field, ast.AnnAssign) and isinstance(field.target, ast.Name):
                            add("configuration_field", name, field.lineno, f"{node.name}.{field.target.id}",
                                annotation=expression(field.annotation), default=expression(field.value))
        categories = [category for category, prefixes in config["feature_directories"].items() if any(name.startswith(p) for p in prefixes)]
        if name.startswith("litellm/llms/") and not name.endswith("/__init__.py"):
            categories.append("provider_module")
        for category in categories:
            add(category, name, 1, "module")
            for node in tree.body:
                if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and not node.name.startswith("_"):
                    add(category, name, node.lineno, node.name, signature=expression(node.args))
                elif isinstance(node, ast.ClassDef):
                    for method in node.body:
                        if isinstance(method, (ast.FunctionDef, ast.AsyncFunctionDef)) and not method.name.startswith("_"):
                            add(category, name, method.lineno, f"{node.name}.{method.name}", signature=expression(method.args))

    readme = (source / "README.md").read_text().splitlines()
    columns = None
    for number, line in enumerate(readme, 1):
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if line.startswith("| Provider"):
            columns = cells[1:]
        elif columns and line.startswith("|") and "✅" in line:
            for api, cell in zip(columns, cells[1:]):
                if "✅" in cell:
                    add("provider_api", "README.md", number, f"{cells[0]} {api}", provider_label=cells[0], api=api.strip("`"))
        elif columns and not line.startswith("|"):
            columns = None

    for name in manifest["files"]:
        if name.startswith("ui/litellm-dashboard/src/") and name.endswith((".tsx", ".ts")):
            if name.endswith("/page.tsx"):
                add("ui_page", name, 1, "page")
            for number, line in enumerate((source / name).read_text().splitlines(), 1):
                match = re.search(r"export\s+(?:async\s+)?function\s+(\w+)", line)
                if match:
                    add("ui_operation", name, number, match[1])
        if name.startswith("litellm-rust/") and name.endswith(".rs"):
            for number, line in enumerate((source / name).read_text().splitlines(), 1):
                if re.search(r"\b(?:todo!|unimplemented!|unsupported\b|TODO\b|FIXME\b)", line, re.I):
                    add("upstream_rust_gap", name, number, f"line-{number}", source_text=line.strip())

    ordered = [features[key] for key in sorted(features)]
    return {"schema_version": 1, "baseline": {key: config[key] for key in ["version", "commit", "archive_sha256"]},
            "discovery_only": True, "counts": dict(sorted(Counter(f["kind"] for f in ordered).items())), "features": ordered}


def main():
    if sys.version_info < (3, 12):
        print("Python 3.12+ is required to parse the pinned Python source", file=sys.stderr)
        return 1
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["generate", "check"])
    args = parser.parse_args()
    config = baseline()
    try:
        verify(config)
        payload = json.dumps(generate(config), ensure_ascii=False, indent=2) + "\n"
        destination = ROOT / config["inventory"]
        if args.action == "check":
            if not destination.exists() or destination.read_text() != payload:
                raise ValueError("inventory is missing or stale; regenerate it")
        else:
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(payload)
    except (ValueError, KeyError, OSError) as error:
        print(str(error), file=sys.stderr)
        return 1
    print(json.dumps({"inventory": str(destination.relative_to(ROOT)), "counts": json.loads(payload)["counts"]}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
