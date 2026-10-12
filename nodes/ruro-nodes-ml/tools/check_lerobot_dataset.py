"""Checks a dataset written by `ml/lerobot-recorder` the way LeRobot v2.1 reads it.

    venv/bin/pip install "datasets<=3.6" pandas pillow
    venv/bin/python nodes/ruro-nodes-ml/tools/check_lerobot_dataset.py <dataset root>

Loads the Parquet files with Hugging Face `datasets` (as
`LeRobotDataset.load_hf_dataset` does), compares the features with the ones
LeRobot derives from meta/info.json, decodes images, and checks indices,
timestamps, episode metadata and statistics. Prints a summary on success.
"""

import json
import sys
from pathlib import Path

import datasets
import numpy as np
import pandas as pd


def expected_features(features):
    # Mirrors lerobot.datasets.utils.get_hf_features_from_features (v2.1).
    out = {}
    for key, ft in features.items():
        if ft["dtype"] == "video":
            continue
        if ft["dtype"] == "image":
            out[key] = datasets.Image()
        elif tuple(ft["shape"]) == (1,):
            out[key] = datasets.Value(dtype=ft["dtype"])
        else:
            out[key] = datasets.Sequence(length=ft["shape"][0], feature=datasets.Value(dtype=ft["dtype"]))
    return datasets.Features(out)


def jsonl(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def main(root):
    root = Path(root)
    info = json.loads((root / "meta/info.json").read_text())
    assert info["codebase_version"] == "v2.1", info["codebase_version"]
    episodes = jsonl(root / "meta/episodes.jsonl")
    tasks = jsonl(root / "meta/tasks.jsonl")
    stats = jsonl(root / "meta/episodes_stats.jsonl")
    assert info["total_episodes"] == len(episodes) == len(stats)
    assert info["total_tasks"] == len(tasks)
    assert info["total_frames"] == sum(e["length"] for e in episodes)
    assert info["splits"] == {"train": f"0:{len(episodes)}"}

    ds = datasets.load_dataset("parquet", data_dir=str(root / "data"), split="train")
    assert ds.features == expected_features(info["features"]), (ds.features, info["features"])
    assert len(ds) == info["total_frames"]

    df = pd.concat(
        pd.read_parquet(root / info["data_path"].format(episode_chunk=e["episode_index"] // info["chunks_size"], episode_index=e["episode_index"]))
        for e in episodes
    )
    assert (df["index"].to_numpy() == np.arange(len(df))).all()
    for e, s in zip(episodes, stats):
        ep = df[df["episode_index"] == e["episode_index"]]
        assert len(ep) == e["length"]
        assert (ep["frame_index"].to_numpy() == np.arange(len(ep))).all()
        assert np.allclose(ep["timestamp"].to_numpy(), np.arange(len(ep)) / info["fps"])
        state = np.stack(ep["observation.state"].to_numpy())
        assert np.allclose(s["stats"]["observation.state/mean"], state.mean(axis=0), atol=1e-5)
        assert np.allclose(s["stats"]["observation.state/std"], state.std(axis=0), atol=1e-5)
        assert s["stats"]["observation.state/count"] == [len(ep)]
        assert [tasks[i]["task"] for i in set(ep["task_index"])] == e["tasks"]

    image_keys = [k for k, f in info["features"].items() if f["dtype"] == "image"]
    for key in image_keys:
        img = ds[0][key]  # PIL image decoded from the embedded PNG
        h, w, c = info["features"][key]["shape"]
        assert img.size == (w, h) and img.mode == "RGB", (img.size, img.mode)
        assert np.array(stats[0]["stats"][f"{key}/mean"]).shape == (3, 1, 1)

    print(f"ok: {len(episodes)} episodes, {len(ds)} frames, features {sorted(ds.features)}")


if __name__ == "__main__":
    main(sys.argv[1])
