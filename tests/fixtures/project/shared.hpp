// Buildscope fixture; tests compile temporary copies.
#pragma once
#include <vector>
#include <algorithm>
template<int N> constexpr unsigned checksum() {
 if constexpr (N == 0) return 0;
 else return N + checksum<N - 1>();
}
template<int N> int transform(int x) {
 if constexpr (N == 0) return x;
 else return transform<N - 1>(x + N);
}
inline int expensive(std::vector<int> values) {
 std::sort(values.begin(), values.end());
 return values.empty() ? 0 : transform<40>(values.front());
}
