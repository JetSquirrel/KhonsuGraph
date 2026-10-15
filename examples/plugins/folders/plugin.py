"""A KhonsuGraph data-source plugin in plain Python (no dependencies).

The host runs `python3 plugin.py describe|test|sync`. `test` and `sync` read
one JSON request line on stdin; every answer is one JSON object per line on
stdout. See docs/plugins.md.
"""
import json
import os
import sys

PROTOCOL = 1


def emit(message):
    sys.stdout.write(json.dumps(message) + "\n")


def describe():
    emit({
        "type": "describe",
        "name": "Folders",
        "version": "0.1.0",
        "description": "A folder tree as Folder and File nodes joined by CONTAINS",
        "config": [
            {"key": "root", "label": "Folder", "kind": "file", "required": True,
             "help": "The folder to read"},
            {"key": "max_files", "label": "File limit", "kind": "number",
             "default": 5000, "help": "Stop after this many files"},
        ],
        "permissions": {"read_files": True, "commands": ["python3"]},
        "labels": ["Folder", "File"],
        "relationship_types": ["CONTAINS"],
    })


def root_of(request):
    root = request.get("config", {}).get("root")
    if not root or not os.path.isdir(root):
        raise ValueError(f"folder not found: {root!r}")
    return os.path.abspath(root)


def node_id(root, path):
    # Stable within the source: the path relative to the root folder.
    rel = os.path.relpath(path, root).replace(os.sep, "/")
    return "." if rel == "." else rel


def sync(request):
    root = root_of(request)
    limit = int(request.get("config", {}).get("max_files") or 5000)
    nodes = edges = files = 0
    for folder, subfolders, names in os.walk(root):
        subfolders.sort()
        folder_id = node_id(root, folder)
        emit({"type": "node", "id": folder_id, "label": "Folder",
              "properties": {"name": os.path.basename(folder) or folder, "path": folder}})
        nodes += 1
        if folder != root:
            parent = node_id(root, os.path.dirname(folder))
            emit({"type": "edge", "id": f"{parent}>{folder_id}", "source": parent,
                  "target": folder_id, "rel_type": "CONTAINS"})
            edges += 1
        for name in sorted(names):
            if files >= limit:
                break
            path = os.path.join(folder, name)
            file_id = node_id(root, path)
            emit({"type": "node", "id": file_id, "label": "File",
                  "properties": {"name": name, "path": path,
                                 "extension": os.path.splitext(name)[1].lstrip("."),
                                 "bytes": os.path.getsize(path)}})
            emit({"type": "edge", "id": f"{folder_id}>{file_id}", "source": folder_id,
                  "target": file_id, "rel_type": "CONTAINS"})
            nodes += 1
            edges += 1
            files += 1
        emit({"type": "progress", "message": folder_id, "current": files, "total": None})
    if files >= limit:
        emit({"type": "log", "level": "warn", "message": f"stopped at {limit} files"})
    emit({"type": "done", "nodes": nodes, "edges": edges})


def main():
    operation = sys.argv[1] if len(sys.argv) > 1 else ""
    if operation == "describe":
        return describe()
    request = json.loads(sys.stdin.readline() or "{}")
    try:
        if operation == "test":
            root = root_of(request)
            emit({"type": "test_result", "ok": True, "message": f"can read {root}"})
        elif operation == "sync":
            sync(request)
        else:
            emit({"type": "error", "message": f"unknown operation {operation!r}"})
    except Exception as err:  # reported to the person, not as a traceback
        emit({"type": "error", "message": str(err)})


if __name__ == "__main__":
    main()
