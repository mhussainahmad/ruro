/*
 * Prints the mjModel/mjData layout table used by the `sim/mujoco` node
 * (src/mujoco/layout.rs) for the MuJoCo headers it is compiled against.
 *
 * MuJoCo has no stable ABI for struct fields, so supporting a new release
 * means generating its entry with this program and adding it to the table:
 *
 *   python -m venv venv && venv/bin/pip install mujoco
 *   INC=$(venv/bin/python -c "import mujoco, os; print(os.path.dirname(mujoco.__file__))")/include
 *   cc -I"$INC" nodes/ruro-nodes-sim/tools/mujoco_layout.c -o /tmp/mujoco_layout && /tmp/mujoco_layout
 */
#include <stddef.h>
#include <stdio.h>
#include <mujoco/mujoco.h>

#define M(f) printf("        model_%s: %zu,\n", #f, offsetof(mjModel, f))
#define D(f) printf("        data_%s: %zu,\n", #f, offsetof(mjData, f))

int main(void) {
  if (sizeof(mjtNum) != 8) return 1;
  printf("    Layout {\n");
  printf("        version: %d,\n", mjVERSION_HEADER);
  printf("        size_bytes: %zu,\n", sizeof(((mjModel*)0)->nq));
  printf("        index_bytes: %zu,\n", sizeof(*((mjModel*)0)->jnt_type));
  M(nq); M(nv); M(nu); M(nbody); M(njnt); M(nsensor); M(nsensordata);
  printf("        model_timestep: %zu,\n", offsetof(mjModel, opt) + offsetof(mjOption, timestep));
  M(jnt_type); M(jnt_qposadr); M(jnt_dofadr); M(sensor_dim); M(sensor_adr);
  D(time); D(qpos); D(qvel); D(ctrl); D(sensordata); D(xpos); D(xquat);
  printf("        obj_body: %d,\n", mjOBJ_BODY);
  printf("        obj_joint: %d,\n", mjOBJ_JOINT);
  printf("        obj_actuator: %d,\n", mjOBJ_ACTUATOR);
  printf("        obj_sensor: %d,\n", mjOBJ_SENSOR);
  printf("        state_qpos: %d,\n", mjSTATE_QPOS);
  printf("        state_qvel: %d,\n", mjSTATE_QVEL);
  printf("    },\n");
  return 0;
}
