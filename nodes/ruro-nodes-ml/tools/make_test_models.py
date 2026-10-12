"""Builds the tiny ONNX models used by the `onnx` feature tests.

    python -m venv venv && venv/bin/pip install onnx numpy
    venv/bin/python nodes/ruro-nodes-ml/tools/make_test_models.py nodes/ruro-nodes-ml/tests/models

The models are hand-assembled graphs of a few operators (a few hundred bytes
each), so the expected outputs can be computed exactly in the tests.
"""

import sys
from pathlib import Path

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper


def save(model, path):
    model.ir_version = 8
    onnx.checker.check_model(model)
    onnx.save(model, path)
    print(f"{path}: {path.stat().st_size} bytes")


def detector(out_dir):
    """`images` [1,3,32,32] -> `output0` [1,6,3]: YOLOv8 layout, 2 classes, 3 anchors.

    Box predictions are constants; the class-0 score of anchor 0 is the mean
    of the input, so a white image (1.0 after scaling) gives score 1.0 and a
    black image gives 0.0. Anchors 1 and 2 overlap (IoU 2/3) with scores 0.9
    and 0.8 for class 1, so NMS keeps only anchor 1.
    """
    boxes = np.array(
        [
            # cx, cy, w, h for anchors 0..2 (model input pixels)
            [8.0, 20.0, 18.0],
            [8.0, 16.0, 16.0],
            [8.0, 10.0, 10.0],
            [8.0, 10.0, 10.0],
        ],
        dtype=np.float32,
    )
    class1 = np.array([[0.0, 0.9, 0.8]], dtype=np.float32)
    nodes = [
        helper.make_node("ReduceMean", ["images"], ["mean"], keepdims=0),  # scalar
        helper.make_node("Reshape", ["mean", "shape_11"], ["mean11"]),
        helper.make_node("Concat", ["mean11", "zeros12"], ["class0"], axis=1),  # [1,3]
        helper.make_node("Concat", ["boxes", "class0", "class1"], ["rows"], axis=0),  # [6,3]
        helper.make_node("Unsqueeze", ["rows", "axis0"], ["output0"]),
    ]
    inits = [
        numpy_helper.from_array(boxes, "boxes"),
        numpy_helper.from_array(class1, "class1"),
        numpy_helper.from_array(np.zeros((1, 2), np.float32), "zeros12"),
        numpy_helper.from_array(np.array([1, 1], np.int64), "shape_11"),
        numpy_helper.from_array(np.array([0], np.int64), "axis0"),
    ]
    graph = helper.make_graph(
        nodes,
        "tiny_detector",
        [helper.make_tensor_value_info("images", TensorProto.FLOAT, [1, 3, 32, 32])],
        [helper.make_tensor_value_info("output0", TensorProto.FLOAT, [1, 6, 3])],
        inits,
    )
    save(helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)]), out_dir / "detector.onnx")


def policy(out_dir):
    """`observation.state` [1,6] + `observation.image` [1,3,8,8] -> `action` [1,2,6].

    action[k] = state + (k + 1) * mean(image): a two-step action chunk.
    """
    nodes = [
        helper.make_node("ReduceMean", ["observation.image"], ["m"], keepdims=0),
        helper.make_node("Mul", ["steps", "m"], ["offsets"]),  # [2,1]
        helper.make_node("Add", ["observation.state", "offsets"], ["chunk"]),  # [2,6]
        helper.make_node("Unsqueeze", ["chunk", "axis0"], ["action"]),
    ]
    inits = [
        numpy_helper.from_array(np.array([[1.0], [2.0]], np.float32), "steps"),
        numpy_helper.from_array(np.array([0], np.int64), "axis0"),
    ]
    graph = helper.make_graph(
        nodes,
        "tiny_policy",
        [
            helper.make_tensor_value_info("observation.state", TensorProto.FLOAT, [1, 6]),
            helper.make_tensor_value_info("observation.image", TensorProto.FLOAT, [1, 3, 8, 8]),
        ],
        [helper.make_tensor_value_info("action", TensorProto.FLOAT, [1, 2, 6])],
        inits,
    )
    save(helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)]), out_dir / "policy.onnx")


if __name__ == "__main__":
    out = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
    out.mkdir(parents=True, exist_ok=True)
    detector(out)
    policy(out)
