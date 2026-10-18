// Buildscope fixture; tests compile temporary copies.
#include "api.hpp"
#include "config.hpp"
int first() { return expensive({3,1,2}) + generated_value; }
