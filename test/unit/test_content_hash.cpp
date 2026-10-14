#include <catch2/catch_test_macros.hpp>

#include <filesystem>
#include <fstream>
#include <unistd.h>

#include "symhow/common/ContentHash.h"

TEST_CASE("content_hash: SHA-256 standard vectors, padding boundaries and binary bytes")
{
    using symhow::common::contentHash;
    REQUIRE(contentHash("") == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    REQUIRE(contentHash("abc") == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    REQUIRE(contentHash("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq") ==
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
    REQUIRE(contentHash(std::string(1000000, 'a')) == "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    REQUIRE(contentHash(std::string(55, 'a')) == "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318");
    REQUIRE(contentHash(std::string(56, 'a')) == "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a");
    REQUIRE(contentHash(std::string(64, 'a')) == "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb");
    REQUIRE(contentHash(std::string(65, 'a')) == "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0");
    REQUIRE(contentHash(std::string("\0\xff\r\n", 4)) == "e9489f37fb3051e9efa1dc916004d7274e7b63975e3209708947267f2393a9be");
    REQUIRE(contentHash("int a;\r\n") != contentHash("int a;\n"));
    REQUIRE(contentHash("int a;") != contentHash("int a;\n"));
}

TEST_CASE("content_hash: streamed files match buffers and read failures are conservative")
{
    using symhow::common::contentHash;
    using symhow::common::fileContentHash;
    const auto dir = std::filesystem::temp_directory_path() / ("symhow_content_hash_" + std::to_string(::getpid()));
    std::filesystem::remove_all(dir);
    std::filesystem::create_directories(dir);
    struct Cleanup
    {
        std::filesystem::path path;
        ~Cleanup() { std::filesystem::remove_all(path); }
    } cleanup{dir};
    const auto file = (dir / "bytes").string();
    for (std::size_t size : {0u, 1u, 16383u, 16384u, 16385u, 32768u})
    {
        std::string bytes(size, '\0');
        for (std::size_t i = 0; i < size; ++i) { bytes[i] = static_cast<char>(i % 256); }
        {
            std::ofstream out(file, std::ios::binary);
            out.write(bytes.data(), bytes.size());
        }
        REQUIRE(fileContentHash(file) == contentHash(bytes));
    }
    REQUIRE_FALSE(fileContentHash((dir / "missing").string()).has_value());
    REQUIRE_FALSE(fileContentHash(dir.string()).has_value());
    std::filesystem::create_symlink(file, dir / "alias");
    REQUIRE(fileContentHash((dir / "alias").string()) == fileContentHash(file));
}
