# Nested messages, arrays of them and empty messages inside.
uint8 KIND_A=1
uint8 KIND_B=2

std_msgs/Header header
uint8 kind
Primitives one
Primitives[2] two
Arrays[] many
Arrays[<=2] few
std_msgs/Empty nothing
std_msgs/Empty[2] nothings
uint8 after_empty
geometry_msgs/Pose[] poses
float64 last
