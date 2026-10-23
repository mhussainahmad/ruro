# Builds the example nodes into one plugin: make -C examples/native
RURO_INCLUDE ?= ../../sdk/include
CFLAGS ?= -O2 -Wall -Wextra
CXXFLAGS ?= -O2 -Wall -Wextra

libexample_nodes.so: lowpass.o threshold.o
	$(CXX) -shared -o $@ $^

lowpass.o: lowpass.c
	$(CC) -std=c11 -fPIC -fvisibility=hidden $(CFLAGS) -I$(RURO_INCLUDE) -c -o $@ $<

threshold.o: threshold.cpp
	$(CXX) -std=c++17 -fPIC -fvisibility=hidden $(CXXFLAGS) -I$(RURO_INCLUDE) -c -o $@ $<

clean:
	rm -f lowpass.o threshold.o libexample_nodes.so
