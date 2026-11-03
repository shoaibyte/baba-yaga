<?php
// Uses old symfony/http-foundation and guzzlehttp/guzzle releases on purpose.

require __DIR__ . '/vendor/autoload.php';

use GuzzleHttp\Client;
use Symfony\Component\HttpFoundation\Request;

$request = Request::createFromGlobals();
$client = new Client();
echo get_class($client), ' ', $request->getMethod(), PHP_EOL;
