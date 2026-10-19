# Fixed, bounded and unbounded arrays of every kind of element.
bool[2] fixed_bool
byte[3] fixed_byte
char[2] fixed_char
float32[3] fixed_f32 [1.0, 2.0, 3.0]
float64[2] fixed_f64
int8[3] fixed_i8
uint8[4] fixed_u8
int16[2] fixed_i16
uint16[2] fixed_u16
int32[2] fixed_i32 [-1, 1]
uint32[2] fixed_u32
int64[2] fixed_i64
uint64[2] fixed_u64
string[2] fixed_s ["a", 'b']
wstring[2] fixed_ws

bool[<=3] bounded_bool
byte[<=4] bounded_byte
uint8[<=5] bounded_u8
int16[<=3] bounded_i16
float32[<=3] bounded_f32
float64[<=3] bounded_f64
int64[<=2] bounded_i64
string[<=3] bounded_s
string<=5[<=2] bounded_bounded_s
wstring[<=2] bounded_ws

bool[] seq_bool
byte[] seq_byte
char[] seq_char
uint8[] seq_u8
int8[] seq_i8
uint16[] seq_u16
int32[] seq_i32 [5, 6, 7]
uint32[] seq_u32
float32[] seq_f32
float64[] seq_f64
uint64[] seq_u64
int64[] seq_i64
string[] seq_s
wstring[] seq_ws
