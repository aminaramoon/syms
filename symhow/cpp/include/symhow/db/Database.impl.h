#pragma once

// Template-method implementation for Database::runChunkedIn(). Split out of
// Database.h purely so that header stays free of <sqlite3.h> (it only
// forward-declares sqlite3/sqlite3_stmt); this file needs the real
// sqlite3_bind_int64() declaration and is included at the bottom of
// Database.h.

#include <sqlite3.h>

#include <algorithm>
#include <stdexcept>

namespace symhow::db
{

template <typename T>
void Database::runChunkedIn(
    const std::string& sqlTemplate,
    const std::vector<T>& ids,
    const std::function<void(sqlite3_stmt*, int)>& extraBind,
    const RowFn& onRow,
    std::size_t chunkSize) const
{
    if (ids.empty())
    {
        return;
    }
    if (chunkSize == 0)
    {
        chunkSize = 500;
    }

    std::size_t pos = 0;
    while (pos < ids.size())
    {
        const std::size_t n = std::min(chunkSize, ids.size() - pos);

        std::string placeholders;
        placeholders.reserve(n * 2);
        for (std::size_t i = 0; i < n; ++i)
        {
            if (i != 0)
            {
                placeholders += ',';
            }
            placeholders += '?';
        }

        std::string sql = sqlTemplate;
        const std::string::size_type marker = sql.find("{IN}");
        if (marker == std::string::npos)
        {
            throw std::runtime_error("Database::runChunkedIn: sqlTemplate is missing the {IN} marker: " + sqlTemplate);
        }
        sql.replace(marker, 4, placeholders);

        const std::size_t chunkStart = pos;
        Binder bind = [&ids, chunkStart, n, &extraBind](sqlite3_stmt* stmt) {
            int idx = 1;
            for (std::size_t i = 0; i < n; ++i)
            {
                sqlite3_bind_int64(stmt, idx++, static_cast<sqlite3_int64>(ids[chunkStart + i]));
            }
            if (extraBind)
            {
                extraBind(stmt, idx);
            }
        };

        run(sql, bind, onRow);
        pos += n;
    }
}

}  // namespace symhow::db
