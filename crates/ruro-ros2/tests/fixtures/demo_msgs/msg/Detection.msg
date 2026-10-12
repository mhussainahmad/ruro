# A detection with a bounding polygon.
std_msgs/Header header
geometry_msgs/Point[] corners
uint8 KIND_BOX=1
uint8 KIND_POLY=2
uint8 kind 1
string<=32 label "unknown"
float32[4] scores [0.1, 0.2, 0.3, 0.4]
Detail detail
Detail[<=3] extra
byte[] blob
