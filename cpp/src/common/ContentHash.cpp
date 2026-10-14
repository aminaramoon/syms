#include "symhow/common/ContentHash.h"

#include <array>
#include <bit>
#include <cstdint>
#include <filesystem>
#include <fstream>

namespace symhow::common
{
namespace
{

// SHA-256 compression and padding as specified in FIPS 180-4.
class Sha256
{
public:
    void append(std::string_view bytes)
    {
        m_bytes += bytes.size();
        for (unsigned char byte : bytes)
        {
            m_block[m_used++] = byte;
            if (m_used == m_block.size())
            {
                compress();
                m_used = 0;
            }
        }
    }

    std::string finish()
    {
        const uint64_t bits = m_bytes * 8;
        m_block[m_used++] = 0x80;
        if (m_used > 56)
        {
            while (m_used < 64) { m_block[m_used++] = 0; }
            compress();
            m_used = 0;
        }
        while (m_used < 56) { m_block[m_used++] = 0; }
        for (int shift = 56; shift >= 0; shift -= 8)
        {
            m_block[m_used++] = static_cast<unsigned char>(bits >> shift);
        }
        compress();
        constexpr char hex[] = "0123456789abcdef";
        std::string result;
        result.reserve(64);
        for (uint32_t word : m_state)
        {
            for (int shift = 28; shift >= 0; shift -= 4)
            {
                result += hex[(word >> shift) & 15];
            }
        }
        return result;
    }

private:
    void compress()
    {
        static constexpr std::array<uint32_t, 64> k = {
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};
        std::array<uint32_t, 64> w{};
        for (std::size_t i = 0; i < 16; ++i)
        {
            for (std::size_t j = 0; j < 4; ++j)
            {
                w[i] = (w[i] << 8) | m_block[4 * i + j];
            }
        }
        for (std::size_t i = 16; i < w.size(); ++i)
        {
            const uint32_t s0 = std::rotr(w[i - 15], 7) ^ std::rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
            const uint32_t s1 = std::rotr(w[i - 2], 17) ^ std::rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16] + s0 + w[i - 7] + s1;
        }
        auto [a, b, c, d, e, f, g, h] = m_state;
        for (std::size_t i = 0; i < w.size(); ++i)
        {
            const uint32_t sum1 = std::rotr(e, 6) ^ std::rotr(e, 11) ^ std::rotr(e, 25);
            const uint32_t choose = (e & f) ^ (~e & g);
            const uint32_t t1 = h + sum1 + choose + k[i] + w[i];
            const uint32_t sum0 = std::rotr(a, 2) ^ std::rotr(a, 13) ^ std::rotr(a, 22);
            const uint32_t majority = (a & b) ^ (a & c) ^ (b & c);
            const uint32_t t2 = sum0 + majority;
            h = g; g = f; f = e; e = d + t1;
            d = c; c = b; b = a; a = t1 + t2;
        }
        const std::array<uint32_t, 8> work{a, b, c, d, e, f, g, h};
        for (std::size_t i = 0; i < work.size(); ++i) { m_state[i] += work[i]; }
    }

    std::array<uint32_t, 8> m_state{
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    std::array<unsigned char, 64> m_block{};
    std::size_t m_used = 0;
    uint64_t m_bytes = 0;
};

}  // namespace

std::string contentHash(std::string_view bytes)
{
    Sha256 hash;
    hash.append(bytes);
    return hash.finish();
}

std::optional<std::string> fileContentHash(const std::string& path)
{
    std::error_code ec;
    if (!std::filesystem::is_regular_file(path, ec) || ec) { return std::nullopt; }
    std::ifstream file(path, std::ios::binary);
    if (!file) { return std::nullopt; }
    Sha256 hash;
    std::array<char, 16384> buffer{};
    while (file.read(buffer.data(), buffer.size()) || file.gcount() > 0)
    {
        hash.append(std::string_view(buffer.data(), static_cast<std::size_t>(file.gcount())));
    }
    if (file.bad() || !file.eof()) { return std::nullopt; }
    return hash.finish();
}

}  // namespace symhow::common
