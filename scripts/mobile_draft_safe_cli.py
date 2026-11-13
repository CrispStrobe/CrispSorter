"""Bounded diagnostics for draft CLIs; never echo records or exception strings."""
import argparse
import json
import subprocess
import sys


class SafeArgumentParser(argparse.ArgumentParser):
    def error(self, message):
        self.exit(2, 'Mobile draft command failed [invalid-arguments]\n')


def failure_code(error):
    if isinstance(error, subprocess.TimeoutExpired):
        return 'external-timeout'
    if isinstance(error, subprocess.CalledProcessError):
        return 'external-command-failed'
    if isinstance(error, (json.JSONDecodeError, UnicodeError)):
        return 'invalid-record'
    if isinstance(error, (ValueError, KeyError, TypeError)):
        return 'invalid-input'
    if isinstance(error, OSError):
        return 'input-output-error'
    return 'internal-error'


def run(main):
    try:
        main()
    except Exception as error:
        print('Mobile draft command failed [' + failure_code(error) + ']', file=sys.stderr)
        return 1
    return 0
