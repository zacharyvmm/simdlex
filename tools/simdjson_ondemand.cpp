#include "simdjson.h"

#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <string>
#include <string_view>
#include <utility>

using simdjson::ondemand::json_type;

static uint64_t bytes_checksum(uint64_t seed, std::string_view bytes) {
  uint64_t checksum = seed ^ bytes.size();
  for (unsigned char byte : bytes) {
    checksum = (checksum << 5 | checksum >> 59) ^ byte;
  }
  return checksum;
}

static std::string_view trim_token(std::string_view token) {
  while (!token.empty()) {
    const char byte = token.back();
    if (byte != ' ' && byte != '\t' && byte != '\n' && byte != '\r') {
      break;
    }
    token.remove_suffix(1);
  }
  return token;
}

static simdjson::error_code traverse(simdjson::ondemand::value value,
                                     uint64_t &checksum) {
  json_type type;
  SIMDJSON_TRY(value.type().get(type));
  switch (type) {
  case json_type::object: {
    checksum += 1;
    simdjson::ondemand::object object;
    SIMDJSON_TRY(value.get_object().get(object));
    for (auto field : object) {
      checksum += bytes_checksum(2, field.escaped_key());
      simdjson::ondemand::value child = field.value();
      SIMDJSON_TRY(traverse(child, checksum));
    }
    checksum += 3;
    return simdjson::SUCCESS;
  }
  case json_type::array: {
    checksum += 4;
    simdjson::ondemand::array array;
    SIMDJSON_TRY(value.get_array().get(array));
    for (auto child : array) {
      simdjson::ondemand::value child_value = child.value();
      SIMDJSON_TRY(traverse(child_value, checksum));
    }
    checksum += 5;
    return simdjson::SUCCESS;
  }
  case json_type::string: {
    std::string_view token = trim_token(value.raw_json_token());
    std::string_view decoded;
    SIMDJSON_TRY(value.get_string().get(decoded));
    checksum += bytes_checksum(6, token.substr(1, token.size() - 2));
    return simdjson::SUCCESS;
  }
  case json_type::number: {
    std::string_view token = trim_token(value.raw_json_token());
    simdjson::ondemand::number number;
    SIMDJSON_TRY(value.get_number().get(number));
    checksum += bytes_checksum(7, token);
    return simdjson::SUCCESS;
  }
  case json_type::boolean: {
    std::string_view token = trim_token(value.raw_json_token());
    bool boolean;
    SIMDJSON_TRY(value.get_bool().get(boolean));
    checksum += bytes_checksum(8 + uint64_t(boolean), token);
    return simdjson::SUCCESS;
  }
  case json_type::null: {
    std::string_view token = trim_token(value.raw_json_token());
    bool is_null;
    SIMDJSON_TRY(value.is_null().get(is_null));
    if (!is_null) {
      return simdjson::INCORRECT_TYPE;
    }
    checksum += bytes_checksum(10, token);
    return simdjson::SUCCESS;
  }
  }
  return simdjson::TAPE_ERROR;
}

int main(int argc, char **argv) {
  if (argc < 4) {
    std::cerr << "usage: simdjson_ondemand <implementation> <iterations> "
                 "<json-file>...\n";
    return 2;
  }

  auto implementation =
      simdjson::get_available_implementations()[std::string(argv[1])];
  if (!implementation || !implementation->supported_by_runtime_system()) {
    std::cerr << "unsupported implementation: " << argv[1] << "\n";
    return 2;
  }
  simdjson::get_active_implementation() = implementation;
  const size_t iterations = std::strtoull(argv[2], nullptr, 10);

  for (int argument = 3; argument < argc; ++argument) {
    simdjson::padded_string input;
    auto load_error = simdjson::padded_string::load(argv[argument]).get(input);
    if (load_error) {
      std::cerr << load_error << ": " << argv[argument] << "\n";
      return 1;
    }
    simdjson::ondemand::parser parser;
    uint64_t checksum = 0;

    const auto start = std::chrono::steady_clock::now();
    for (size_t iteration = 0; iteration < iterations; ++iteration) {
      simdjson::ondemand::document document;
      auto parse_error = parser.iterate(input).get(document);
      if (parse_error) {
        std::cerr << parse_error << ": " << argv[argument] << "\n";
        return 1;
      }
      simdjson::ondemand::value root = document;
      auto traversal_error = traverse(root, checksum);
      if (traversal_error) {
        std::cerr << traversal_error << ": " << argv[argument] << "\n";
        return 1;
      }
    }
    const auto elapsed = std::chrono::duration<double>(
        std::chrono::steady_clock::now() - start);
    const double gbps =
        double(input.size()) * double(iterations) / elapsed.count() / 1e9;
    std::cout << argv[argument] << "\timplementation=" << argv[1]
              << "\ttraversal=" << gbps << " GB/s"
              << "\tchecksum=" << checksum << "\n";
  }
}
