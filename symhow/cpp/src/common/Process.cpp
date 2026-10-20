#include "symhow/common/Process.h"

#include <poll.h>
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>
#include <cerrno>
#include <cstring>
#include <vector>

extern char** environ;

namespace symhow::common::process
{

namespace
{

// Drains both pipes concurrently via poll() until each has hit EOF, rather
// than reading `outFd` to EOF before ever touching `errFd` -- a child that
// writes more than the pipe buffer's worth (typically 64 KiB) to stderr
// before/instead of stdout would otherwise block forever on its write()
// while we're still blocked reading the OTHER pipe.
void drainBoth(int outFd, int errFd, std::string& outStr, std::string& errStr)
{
    struct Fd
    {
        int fd;
        std::string* dest;
        bool open;
    };
    Fd fds[2] = {{outFd, &outStr, true}, {errFd, &errStr, true}};

    char buf[4096];
    while (fds[0].open || fds[1].open)
    {
        struct pollfd pfds[2];
        int nfds = 0;
        int idxByPfd[2];
        for (int i = 0; i < 2; ++i)
        {
            if (fds[i].open)
            {
                pfds[nfds].fd = fds[i].fd;
                pfds[nfds].events = POLLIN;
                pfds[nfds].revents = 0;
                idxByPfd[nfds] = i;
                ++nfds;
            }
        }
        if (nfds == 0)
        {
            break;
        }

        const int rc = ::poll(pfds, static_cast<nfds_t>(nfds), -1);
        if (rc < 0)
        {
            if (errno == EINTR)
            {
                continue;
            }
            break;
        }

        for (int p = 0; p < nfds; ++p)
        {
            if ((pfds[p].revents & (POLLIN | POLLHUP | POLLERR)) == 0)
            {
                continue;
            }
            Fd& f = fds[idxByPfd[p]];
            const ssize_t n = ::read(f.fd, buf, sizeof(buf));
            if (n > 0)
            {
                f.dest->append(buf, static_cast<std::size_t>(n));
            }
            else
            {
                f.open = false;
            }
        }
    }
}

}  // namespace

Result run(const std::vector<std::string>& argv, const std::string* cwd)
{
    Result result;
    if (argv.empty())
    {
        result.err = "symhow::common::process::run: empty argv";
        return result;
    }

    int outPipe[2] = {-1, -1};
    int errPipe[2] = {-1, -1};
    if (::pipe(outPipe) != 0 || ::pipe(errPipe) != 0)
    {
        result.err = std::string("pipe() failed: ") + std::strerror(errno);
        return result;
    }

    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_addclose(&actions, outPipe[0]);
    posix_spawn_file_actions_addclose(&actions, errPipe[0]);
    posix_spawn_file_actions_adddup2(&actions, outPipe[1], STDOUT_FILENO);
    posix_spawn_file_actions_adddup2(&actions, errPipe[1], STDERR_FILENO);
    posix_spawn_file_actions_addclose(&actions, outPipe[1]);
    posix_spawn_file_actions_addclose(&actions, errPipe[1]);
#if defined(__GLIBC__)
    if (cwd != nullptr && !cwd->empty())
    {
        posix_spawn_file_actions_addchdir_np(&actions, cwd->c_str());
    }
#elif defined(__APPLE__)
    // macOS (10.15+) ships posix_spawn_file_actions_addchdir() as the
    // canonical name (its own _np alias is deprecated); same race-free
    // chdir-before-exec semantics as the glibc extension above.
    if (cwd != nullptr && !cwd->empty())
    {
        posix_spawn_file_actions_addchdir(&actions, cwd->c_str());
    }
#else
    // posix_spawn_file_actions_addchdir_np is a glibc extension; on other
    // libcs (musl's own addchdir_np landed separately/later, and it's not
    // portable to rely on) there is no race-free way to chdir the child
    // before exec via posix_spawn. Fail loudly rather than silently
    // ignoring `cwd` and running the child in the wrong directory.
    if (cwd != nullptr && !cwd->empty())
    {
        posix_spawn_file_actions_destroy(&actions);
        ::close(outPipe[0]);
        ::close(outPipe[1]);
        ::close(errPipe[0]);
        ::close(errPipe[1]);
        result.exitCode = -1;
        result.err = "symhow::common::process::run: cwd is not supported on this platform (no posix_spawn chdir extension)";
        return result;
    }
#endif

    std::vector<char*> cargv;
    cargv.reserve(argv.size() + 1);
    for (const auto& a : argv)
    {
        cargv.push_back(const_cast<char*>(a.c_str()));
    }
    cargv.push_back(nullptr);

    pid_t pid = 0;
    const int rc = posix_spawnp(&pid, argv[0].c_str(), &actions, nullptr, cargv.data(), environ);
    posix_spawn_file_actions_destroy(&actions);

    ::close(outPipe[1]);
    ::close(errPipe[1]);

    if (rc != 0)
    {
        ::close(outPipe[0]);
        ::close(errPipe[0]);
        result.exitCode = -1;
        result.err = std::string("failed to spawn '") + argv[0] + "': " + std::strerror(rc);
        return result;
    }

    drainBoth(outPipe[0], errPipe[0], result.out, result.err);
    ::close(outPipe[0]);
    ::close(errPipe[0]);

    int status = 0;
    if (::waitpid(pid, &status, 0) < 0)
    {
        result.exitCode = -1;
        result.err += std::string("\nwaitpid() failed: ") + std::strerror(errno);
        return result;
    }

    if (WIFEXITED(status))
    {
        result.exitCode = WEXITSTATUS(status);
    }
    else
    {
        result.exitCode = -1;
        result.err += "\nchild process terminated abnormally";
    }
    return result;
}

}  // namespace symhow::common::process
