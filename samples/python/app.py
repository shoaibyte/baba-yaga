"""Imports old requests, urllib3 and jinja2 releases on purpose."""

import jinja2
import requests
import urllib3

if __name__ == "__main__":
    print(jinja2.Template("{{ name }}").render(name="sample"))
    print(requests.__version__, urllib3.__version__)
